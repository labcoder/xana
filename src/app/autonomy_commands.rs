//! Application composition for typed durable-work controls. CLI, suspended chat,
//! and Desktop delegate here instead of duplicating validation or persistence.
use crate::{
    autonomy::{self, Action, Job, JobEdit, JobState, Schedule, host, runner},
    cli::{AutonomyArgs, AutonomyCommand, CreateTask, HostCommand},
    paths::XanaPaths,
    storage::ProtectedStore,
};
use anyhow::{Context, Result, ensure};
use std::io::Write;
use uuid::Uuid;

pub(crate) fn create(paths: &XanaPaths, args: CreateTask) -> Result<Job> {
    let store = store(paths)?;
    let job = prepare(paths, &store, args)?;
    store.autonomy_create(job)
}

/// Resolve and validate the same exact task for every client, without saving it
/// or fetching credentials/status. File sources capture only a bounded baseline.
pub(crate) fn prepare(paths: &XanaPaths, store: &ProtectedStore, args: CreateTask) -> Result<Job> {
    ensure!(
        args.authorize,
        "schedule creation requires explicit --authorize after reviewing exact task scope"
    );
    let now = autonomy::now()?;
    let expires_at = args.expires.parse::<jiff::Timestamp>()?.as_second();
    ensure!(expires_at > now, "authority expiry must be in the future");
    let scope = runner::resolve_scope(paths, &args.workspace, &args.profile, args.project)?;
    ensure!(
        args.reviewed_route
            .as_ref()
            .is_none_or(|digest| digest == &scope.configuration_digest),
        "reviewed route changed; inspect the new Profile and recipient before creating work"
    );
    let action = match (args.reminder, args.prompt) {
        (Some(text), None) => Action::Reminder { text },
        (None, Some(prompt)) => Action::NativeTask {
            prompt,
            workspace_reads: args.workspace_reads,
        },
        _ => anyhow::bail!("select exactly one reminder or native task"),
    };
    let trigger = match (args.watch_root, args.github_run, args.github_credential) {
        (Some(root), None, None) => Some(autonomy::triggers::Trigger::Files(
            autonomy::triggers::files::FileTrigger::create(
                store,
                &root,
                &scope.workspace,
                &[
                    paths.data_dir().to_owned(),
                    paths.runtime_dir().to_owned(),
                    paths
                        .config_file()
                        .parent()
                        .context("config parent missing")?
                        .to_owned(),
                ],
            )?,
        )),
        (None, Some(run), Some(reference)) => {
            let (kind, name) = reference
                .split_once(':')
                .context("credential reference must be env:NAME or stored:ID")?;
            let credential = match kind {
                "env" => crate::config::CredentialReference::Environment {
                    variable: name.into(),
                },
                "stored" => crate::config::CredentialReference::Stored { id: name.into() },
                _ => anyhow::bail!("credential reference must be env:NAME or stored:ID"),
            };
            Some(autonomy::triggers::Trigger::Github(
                autonomy::triggers::github::GithubTrigger::create(&run, credential)?,
            ))
        }
        (None, None, None) => None,
        _ => anyhow::bail!("select exactly one selected-file or named-CI trigger"),
    };
    let schedule = match (args.at, args.daily, args.timezone, trigger.as_ref()) {
        (None, None, None, Some(trigger)) => Schedule::Triggered {
            poll_seconds: if matches!(trigger, autonomy::triggers::Trigger::Files(_)) {
                5
            } else {
                60
            },
        },
        (Some(at), None, None, None) => Schedule::Once {
            at: at.parse::<jiff::Timestamp>()?.as_second(),
        },
        (None, Some(time), Some(timezone), None) => {
            let (hour, minute) = time.split_once(':').context("daily time must be HH:MM")?;
            ensure!(
                hour.len() == 2 && minute.len() == 2,
                "daily time must be HH:MM"
            );
            Schedule::Daily {
                timezone,
                hour: hour.parse()?,
                minute: minute.parse()?,
            }
        }
        _ => anyhow::bail!("select one at instant or daily time and IANA timezone"),
    };
    let next = schedule.first(now)?;
    ensure!(
        next.at < expires_at,
        "first occurrence must precede authority expiry"
    );
    let job = Job {
        id: Uuid::new_v4(),
        revision: 1,
        conversation: Uuid::new_v4(),
        name: args.name,
        scope,
        action,
        budget: Default::default(),
        schedule,
        trigger,
        expires_at,
        authorized: true,
        state: JobState::Ready,
        not_before: next.at,
        next,
        occurrence: None,
        pause_after_run: false,
        last_receipt: None,
    };
    job.validate()?;
    Ok(job)
}

pub(crate) fn store(paths: &XanaPaths) -> Result<ProtectedStore> {
    ProtectedStore::configured(paths.data_dir())?.context(
        "durable schedules require unlocked protected storage; no plaintext state was created",
    )
}

pub(super) async fn run(args: AutonomyArgs, paths: &XanaPaths) -> Result<()> {
    match args.command {
        AutonomyCommand::Host {
            command: HostCommand::Run { from_startup, home },
        } => {
            let startup_paths = from_startup
                .then(|| XanaPaths::resolve(home.map(Into::into)))
                .transpose()?;
            let paths = startup_paths.as_ref().unwrap_or(paths);
            let store = store(paths)?;
            let learner = super::memory_commands::learning_worker(paths, &store)?;
            host::run(paths, store, from_startup, learner).await
        }
        AutonomyCommand::Host {
            command: HostCommand::Observe,
        } => {
            let mut observer =
                crate::local_host::connect_observer(paths.runtime_dir(), paths.data_dir()).await?;
            println!("{}", serde_json::to_string_pretty(observer.snapshot())?);
            loop {
                tokio::select! {
                    result=tokio::signal::ctrl_c()=>{result?;return Ok(());}
                    event=observer.next()=>{println!("{}",serde_json::to_string(&event?)?);}
                }
            }
        }
        command => control(
            AutonomyArgs { command },
            paths,
            &mut std::io::stdout().lock(),
        ),
    }
}

pub(super) fn control<W: Write>(
    args: AutonomyArgs,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    let value = execute(args.command, paths)?;
    writeln!(output, "{}", serde_json::to_string_pretty(&value)?)?;
    Ok(())
}

pub(crate) fn execute(command: AutonomyCommand, paths: &XanaPaths) -> Result<serde_json::Value> {
    if let AutonomyCommand::Create(args) = command {
        return Ok(serde_json::to_value(create(paths, *args)?)?);
    }
    let store = store(paths)?;
    Ok(match command {
        AutonomyCommand::Overview { after } => {
            serde_json::to_value(autonomy::supervision::page(&store, after)?)?
        }
        AutonomyCommand::Review { id } => serde_json::to_value(autonomy::supervision::review(
            paths,
            &store,
            &store.autonomy_job(id)?,
        )?)?,
        AutonomyCommand::List { after } => serde_json::to_value(store.autonomy_page(after)?)?,
        AutonomyCommand::Show { id } => serde_json::to_value(store.autonomy_job(id)?)?,
        AutonomyCommand::Receipts { id, after } => {
            serde_json::to_value(store.autonomy_receipts(id, after)?)?
        }
        AutonomyCommand::Pause { id, revision } => serde_json::to_value(store.autonomy_edit(
            id,
            revision,
            JobEdit::Pause,
            autonomy::now()?,
        )?)?,
        AutonomyCommand::Resume {
            id,
            revision,
            review_unknown,
        } => serde_json::to_value(store.autonomy_edit(
            id,
            revision,
            JobEdit::Resume { review_unknown },
            autonomy::now()?,
        )?)?,
        AutonomyCommand::Cancel { id, revision } => serde_json::to_value(store.autonomy_edit(
            id,
            revision,
            JobEdit::Cancel,
            autonomy::now()?,
        )?)?,
        AutonomyCommand::Host { command } => match command {
            HostCommand::Status => {
                serde_json::json!({"policy":store.autonomy_policy()?,"discovery":format!("{:?}",crate::local_host::inspect_descriptor_health(paths.runtime_dir(),paths.data_dir()).map_err(anyhow::Error::msg)?),"jobs":host::summaries(&store)?,"stop_impact":store.autonomy_stop_impact()?})
            }
            HostCommand::Start { revision } => {
                let policy = host::policy_edit(&store, revision, Some(true), None, false, false)?;
                serde_json::json!({"policy":policy,"receipt":host::detach(paths)?})
            }
            HostCommand::Stop { revision, lock } => {
                let policy = host::policy_edit(&store, revision, None, None, true, lock)?;
                serde_json::json!({"policy":policy,"stop_impact":store.autonomy_stop_impact()?,"receipt":"Shutdown requested for the named active job. The owning host records attached client identities when it observes this revision; absent clients means unacknowledged, not zero. All queued schedules stop admitting. Inspect host status and terminal receipts; effects remain uncertain until then"})
            }
            HostCommand::Disable { revision } => serde_json::to_value(host::policy_edit(
                &store,
                revision,
                Some(false),
                None,
                true,
                false,
            )?)?,
            HostCommand::Startup { revision, enable } => {
                serde_json::json!({"policy":autonomy::startup::change(&store,paths,revision,enable)?,"receipt":if enable {"Per-user login registration installed and startup authorized. No host was launched now"} else {"Startup authority revoked and the matching per-user registration removed"}})
            }
            HostCommand::Run { .. } | HostCommand::Observe => anyhow::bail!(
                "run/observe require their dedicated CLI process; status/start/stop remain available here"
            ),
        },
        AutonomyCommand::Create(_) => unreachable!(),
    })
}
