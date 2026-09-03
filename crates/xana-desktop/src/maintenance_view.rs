//! Focused graphical Doctor, migration, diagnostics, support, and reset flows.

use gpui::{
    AnyElement, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Role, Task, Window, div, prelude::*,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};
use std::{collections::BTreeSet, path::PathBuf};
use xana::desktop::{
    DesktopControlPlane, DesktopDiagnosticsSnapshot, DesktopDoctorSeverity, DesktopDoctorSnapshot,
    DesktopMigrationSnapshot, DesktopResetPlan, DesktopResetScope,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaintenanceTab {
    Doctor,
    Migration,
    Reset,
    Diagnostics,
}

impl MaintenanceTab {
    const ALL: [Self; 4] = [
        Self::Doctor,
        Self::Migration,
        Self::Reset,
        Self::Diagnostics,
    ];

    const fn id(self) -> &'static str {
        match self {
            Self::Doctor => "doctor",
            Self::Migration => "migration",
            Self::Reset => "reset",
            Self::Diagnostics => "diagnostics",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Doctor => "Doctor",
            Self::Migration => "Migration",
            Self::Reset => "Reset",
            Self::Diagnostics => "Diagnostics & support",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MaintenanceViewEvent {
    Close,
    ConfigurationChanged,
}

pub(crate) struct MaintenanceView {
    control: DesktopControlPlane,
    selected_tab: MaintenanceTab,
    doctor: Option<DesktopDoctorSnapshot>,
    migration: Result<DesktopMigrationSnapshot, String>,
    diagnostics: Result<DesktopDiagnosticsSnapshot, String>,
    reset_scopes: BTreeSet<DesktopResetScope>,
    reset_plan: Option<DesktopResetPlan>,
    repair_reviewed: bool,
    migration_reviewed: bool,
    files_confirmed: bool,
    credentials_confirmed: bool,
    busy: Option<String>,
    error: Option<String>,
    receipt: Option<String>,
    _task: Option<Task<()>>,
}

impl MaintenanceView {
    pub(crate) fn new(
        control: DesktopControlPlane,
        selected_tab: MaintenanceTab,
        migration: Result<DesktopMigrationSnapshot, String>,
        diagnostics: Result<DesktopDiagnosticsSnapshot, String>,
        diagnose_on_open: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            control,
            selected_tab,
            doctor: None,
            migration,
            diagnostics,
            reset_scopes: BTreeSet::from([DesktopResetScope::Setup]),
            reset_plan: None,
            repair_reviewed: false,
            migration_reviewed: false,
            files_confirmed: false,
            credentials_confirmed: false,
            busy: None,
            error: None,
            receipt: None,
            _task: None,
        };
        if diagnose_on_open {
            view.diagnose(false, cx);
        }
        view
    }

    pub(crate) fn diagnose(&mut self, probe_connections: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let control = self.control.clone();
        self.busy = Some(
            if probe_connections {
                "Diagnosing Xana and probing configured connections…"
            } else {
                "Diagnosing local Xana state…"
            }
            .to_owned(),
        );
        self.error = None;
        self.receipt = None;
        self.repair_reviewed = false;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.doctor_snapshot(probe_connections).await })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(snapshot) => this.doctor = Some(snapshot),
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn apply_repairs(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.doctor else { return };
        if self.busy.is_some() || !self.repair_reviewed || snapshot.repairable_codes.is_empty() {
            return;
        }
        let reviewed = snapshot.repairable_codes.clone();
        let control = self.control.clone();
        self.busy = Some("Applying reviewed deterministic repairs…".to_owned());
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.apply_doctor_repairs(&reviewed).await })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.repair_reviewed = false;
                match result {
                    Ok(receipt) => {
                        let failed = receipt
                            .results
                            .iter()
                            .filter(|result| result.failure.is_some())
                            .count();
                        this.receipt = Some(format!(
                            "{} · {} result(s), {failed} failed; diagnose again to verify",
                            receipt.semantic_code,
                            receipt.results.len()
                        ));
                        this.doctor = None;
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn refresh_migration(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let control = self.control.clone();
        self.busy = Some("Inspecting migration state…".to_owned());
        self.migration_reviewed = false;
        self.error = None;
        self.receipt = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.migration_snapshot() })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.migration = result.map_err(|error| error.message);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn apply_migration(&mut self, cx: &mut Context<Self>) {
        let Ok(reviewed) = &self.migration else {
            return;
        };
        if self.busy.is_some() || !self.migration_reviewed || !reviewed.requires_apply {
            return;
        }
        let reviewed = reviewed.clone();
        let control = self.control.clone();
        self.busy = Some("Applying reviewed atomic migration…".to_owned());
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let receipt = control.apply_migration(&reviewed)?;
                    let snapshot = control.migration_snapshot()?;
                    Ok::<_, xana::desktop::DesktopError>((receipt, snapshot))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.migration_reviewed = false;
                match result {
                    Ok((receipt, snapshot)) => {
                        this.receipt = Some(format!(
                            "{} · backup {} · {} private record(s) initialized · {} migrated",
                            receipt.semantic_code,
                            receipt.backup_path,
                            receipt.initialized_private_records,
                            receipt.migrated_private_records
                        ));
                        this.migration = Ok(snapshot);
                        cx.emit(MaintenanceViewEvent::ConfigurationChanged);
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn toggle_reset_scope(&mut self, scope: DesktopResetScope, cx: &mut Context<Self>) {
        if !self.reset_scopes.remove(&scope) {
            self.reset_scopes.insert(scope);
        }
        self.reset_plan = None;
        self.files_confirmed = false;
        self.credentials_confirmed = false;
        self.error = None;
        self.receipt = None;
        cx.notify();
    }

    fn preview_reset(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.reset_scopes.is_empty() {
            return;
        }
        let control = self.control.clone();
        let scopes = self.reset_scopes.iter().copied().collect::<Vec<_>>();
        self.busy = Some("Inspecting exact reset targets…".to_owned());
        self.error = None;
        self.receipt = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.reset_plan(&scopes) })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.files_confirmed = false;
                this.credentials_confirmed = false;
                match result {
                    Ok(plan) => this.reset_plan = Some(plan),
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn execute_reset(&mut self, cx: &mut Context<Self>) {
        let Some(reviewed) = &self.reset_plan else {
            return;
        };
        if self.busy.is_some()
            || (reviewed.requires_files_confirmation && !self.files_confirmed)
            || (reviewed.requires_credentials_confirmation && !self.credentials_confirmed)
        {
            return;
        }
        let reviewed = reviewed.clone();
        let files_confirmed = self.files_confirmed;
        let credentials_confirmed = self.credentials_confirmed;
        let control = self.control.clone();
        self.busy = Some("Resetting only the reviewed Xana-owned state…".to_owned());
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    control.execute_reset(&reviewed, files_confirmed, credentials_confirmed)
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(receipt) => {
                        this.receipt = Some(format!(
                            "{} · {} path(s) removed · {} credential(s) removed",
                            receipt.semantic_code,
                            receipt.removed.len(),
                            receipt.removed_credentials.len()
                        ));
                        this.reset_plan = None;
                        this.files_confirmed = false;
                        this.credentials_confirmed = false;
                        cx.emit(MaintenanceViewEvent::ConfigurationChanged);
                    }
                    Err(error) => self::set_error(this, error.message),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn refresh_diagnostics(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let control = self.control.clone();
        self.busy = Some("Refreshing diagnostics…".to_owned());
        self.error = None;
        self.receipt = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { control.diagnostics_snapshot() })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.diagnostics = result.map_err(|error| error.message);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn export_support_bundle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let selection = cx.prompt_for_new_path(&directory, Some("xana-support.json"));
        let control = self.control.clone();
        cx.spawn_in(window, async move |this, cx| {
            let path = match selection.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    _ = this.update_in(cx, |this, _, cx| {
                        this.error =
                            Some(format!("Could not choose an export destination: {error}"));
                        cx.notify();
                    });
                    return;
                }
                Err(error) => {
                    _ = this.update_in(cx, |this, _, cx| {
                        this.error = Some(format!("Export destination picker stopped: {error}"));
                        cx.notify();
                    });
                    return;
                }
            };
            _ = this.update_in(cx, |this, _, cx| {
                this.busy = Some("Creating a bounded metadata-only support bundle…".to_owned());
                this.error = None;
                cx.notify();
            });
            let result = cx
                .background_executor()
                .spawn(async move { control.export_support_bundle(path) })
                .await;
            _ = this.update_in(cx, |this, _, cx| {
                this.busy = None;
                match result {
                    Ok(path) => this.receipt = Some(format!("Support bundle created: {path}")),
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_doctor(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let findings = self
            .doctor
            .as_ref()
            .map(|snapshot| snapshot.findings.clone())
            .unwrap_or_default();
        v_flex()
            .id("xana-maintenance")
            .role(Role::Region)
            .aria_label("Diagnose and recover Xana")
            .size_full()
            .min_h_0()
            .gap(tokens.spacing.md)
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("maintenance-diagnose")
                            .label("Diagnose local state")
                            .primary()
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.diagnose(false, cx))),
                    )
                    .child(
                        Button::new("maintenance-probe")
                            .label("Diagnose + probe connections")
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.diagnose(true, cx))),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Diagnosis is read-only. Connection probes are opt-in network activity."),
            )
            .child(
                v_flex()
                    .id("maintenance-doctor-findings")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap(tokens.spacing.sm)
                    .when(findings.is_empty(), |list| {
                        list.child("Run diagnosis to inspect setup, storage, runtime ownership, integrations, and local presentation.")
                    })
                    .children(findings.into_iter().map(|finding| {
                        let color = match finding.severity {
                            DesktopDoctorSeverity::Error => cx.theme().danger,
                            DesktopDoctorSeverity::Warning => cx.theme().warning,
                            DesktopDoctorSeverity::Ok => cx.theme().success,
                            DesktopDoctorSeverity::Info => cx.theme().muted_foreground,
                        };
                        v_flex()
                            .p(tokens.spacing.md)
                            .gap(tokens.spacing.xs)
                            .rounded(tokens.radius.md)
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(div().text_color(color).child(format!("{} · {:?}", finding.code, finding.severity)))
                            .child(finding.summary)
                            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(finding.evidence))
                            .when_some(finding.action, |card, action| card.child(format!("Next: {action}")))
                            .when(finding.repairable, |card| card.child(div().text_color(cx.theme().success).child("Deterministic repair available")))
                    })),
            )
            .when(
                self.doctor
                    .as_ref()
                    .is_some_and(|snapshot| !snapshot.repairable_codes.is_empty()),
                |panel| {
                    panel.child(
                        h_flex()
                            .gap(tokens.spacing.sm)
                            .child(
                                Button::new("maintenance-review-repairs")
                                    .label(if self.repair_reviewed { "Repairs reviewed" } else { "Review safe repairs" })
                                    .selected(self.repair_reviewed)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.repair_reviewed = !this.repair_reviewed;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("maintenance-apply-repairs")
                                    .label("Apply exact repairs")
                                    .danger()
                                    .disabled(!self.repair_reviewed || self.busy.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| this.apply_repairs(cx))),
                            ),
                    )
                },
            )
            .into_any_element()
    }

    fn render_migration(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let content = match &self.migration {
            Err(error) => v_flex()
                .gap(tokens.spacing.md)
                .child(div().text_color(cx.theme().danger).child(error.clone()))
                .child("Migration remains unavailable until the configuration can be identified as a supported older schema.")
                .into_any_element(),
            Ok(plan) => v_flex()
                .gap(tokens.spacing.md)
                .child(format!("Schema {} → {}", plan.source_version, plan.target_version))
                .child(if plan.requires_apply { "Migration required" } else { "Configuration and private records are current" })
                .when(plan.private_recovery_pending, |panel| panel.child(div().text_color(cx.theme().warning).child("An interrupted private-state transaction will be recovered by the reviewed migration.")))
                .children(plan.private_records.iter().map(|record| {
                    div().text_sm().child(format!("{} · {}{}", record.name, record.status, record.version.map(|value| format!(" · v{value}")).unwrap_or_default()))
                }))
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Apply writes durable private records first and commits the backed-up configuration version last. Retry is idempotent after interruption."))
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .gap(tokens.spacing.lg)
            .child(content)
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("maintenance-refresh-migration")
                            .label("Refresh preview")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_migration(cx))),
                    )
                    .child(
                        Button::new("maintenance-review-migration")
                            .label(if self.migration_reviewed {
                                "Migration reviewed"
                            } else {
                                "Review migration"
                            })
                            .selected(self.migration_reviewed)
                            .disabled(
                                self.migration.as_ref().is_err_and(|_| true)
                                    || self
                                        .migration
                                        .as_ref()
                                        .is_ok_and(|plan| !plan.requires_apply),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.migration_reviewed = !this.migration_reviewed;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("maintenance-apply-migration")
                            .label("Apply with backup")
                            .primary()
                            .disabled(!self.migration_reviewed || self.busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.apply_migration(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_reset(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let plan = self.reset_plan.clone();
        v_flex()
            .size_full()
            .min_h_0()
            .gap(tokens.spacing.md)
            .child(div().text_color(cx.theme().danger).child("Reset removes only reviewed Xana-owned state. It never removes workspace files or provider-owned history."))
            .child(
                h_flex().flex_wrap().gap(tokens.spacing.xs).children(
                    DesktopResetScope::ALL.into_iter().map(|scope| {
                        Button::new(format!("maintenance-reset-scope-{}", scope.id()))
                            .label(scope.id())
                            .selected(self.reset_scopes.contains(&scope))
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_reset_scope(scope, cx)))
                    }),
                ),
            )
            .child(
                Button::new("maintenance-preview-reset")
                    .label("Review exact reset plan")
                    .disabled(self.reset_scopes.is_empty() || self.busy.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.preview_reset(cx))),
            )
            .when_some(plan, |panel, plan| {
                panel.child(
                    v_flex()
                        .id("maintenance-reset-plan")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .gap(tokens.spacing.sm)
                        .child("Will remove")
                        .when(plan.targets.is_empty() && plan.credential_ids.is_empty(), |items| items.child("Selected Xana state is already clear."))
                        .children(plan.targets.iter().map(|target| div().text_sm().child(format!("{} · {}", target.label, target.path))))
                        .children(plan.credential_ids.iter().map(|id| div().text_sm().text_color(cx.theme().warning).child(format!("OS credential · {id}"))))
                        .child("Will preserve")
                        .children(plan.preserved.iter().map(|item| div().text_sm().text_color(cx.theme().muted_foreground).child(item.clone()))),
                )
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(tokens.spacing.sm)
                        .when(plan.requires_files_confirmation, |actions| actions.child(
                            Button::new("maintenance-confirm-files")
                                .label(if self.files_confirmed { "Filesystem removal confirmed" } else { "Confirm filesystem removal" })
                                .selected(self.files_confirmed)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.files_confirmed = !this.files_confirmed;
                                    cx.notify();
                                })),
                        ))
                        .when(plan.requires_credentials_confirmation, |actions| actions.child(
                            Button::new("maintenance-confirm-credentials")
                                .label(if self.credentials_confirmed { "Credential deletion confirmed" } else { "Separately confirm credential deletion" })
                                .danger()
                                .selected(self.credentials_confirmed)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.credentials_confirmed = !this.credentials_confirmed;
                                    cx.notify();
                                })),
                        ))
                        .child(
                            Button::new("maintenance-execute-reset")
                                .label("Reset reviewed state")
                                .danger()
                                .disabled(
                                    self.busy.is_some()
                                        || (plan.requires_files_confirmation && !self.files_confirmed)
                                        || (plan.requires_credentials_confirmation && !self.credentials_confirmed),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.execute_reset(cx))),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_diagnostics(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tokens = cx.theme().semantic_tokens();
        let content = match &self.diagnostics {
            Err(error) => div()
                .text_color(cx.theme().danger)
                .child(error.clone())
                .into_any_element(),
            Ok(snapshot) => v_flex()
                .gap(tokens.spacing.md)
                .child(format!(
                    "Logging: {}",
                    if snapshot.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ))
                .child(format!("Logs: {}", snapshot.log_directory))
                .child(format!("Crashes: {}", snapshot.crash_directory))
                .child(format!(
                    "{} retained file(s) · {} bytes",
                    snapshot.retained_files, snapshot.retained_bytes
                ))
                .child(format!(
                    "Retention compliant: {} · private permissions: {}",
                    snapshot.retention_compliant, snapshot.permissions_private
                ))
                .child(format!(
                    "Stale runs: {} · invalid reports: {} · dropped events: {} · writer faults: {}",
                    snapshot.stale_markers,
                    snapshot.invalid_reports,
                    snapshot.dropped_events,
                    snapshot.writer_faults
                ))
                .child("Recent diagnostic files")
                .children(snapshot.entries.iter().take(64).map(|entry| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} · {} · {} bytes",
                            entry.kind, entry.name, entry.bytes
                        ))
                }))
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .gap(tokens.spacing.md)
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Support exports contain bounded metadata only: no prompts, transcripts, tool arguments, credentials, or file contents."))
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .child(Button::new("maintenance-refresh-diagnostics").label("Refresh").on_click(cx.listener(|this, _, _, cx| this.refresh_diagnostics(cx))))
                    .child(Button::new("maintenance-export-support").label("Export support bundle…").disabled(self.busy.is_some()).on_click(cx.listener(|this, _, window, cx| this.export_support_bundle(window, cx)))),
            )
            .child(div().flex_1().min_h_0().overflow_y_scrollbar().child(content))
            .into_any_element()
    }
}

impl EventEmitter<MaintenanceViewEvent> for MaintenanceView {}

impl Render for MaintenanceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.theme().semantic_tokens();
        let content = match self.selected_tab {
            MaintenanceTab::Doctor => self.render_doctor(cx),
            MaintenanceTab::Migration => self.render_migration(cx),
            MaintenanceTab::Reset => self.render_reset(cx),
            MaintenanceTab::Diagnostics => self.render_diagnostics(window, cx),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap(tokens.spacing.md)
                    .p(tokens.spacing.md)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().text_2xl().child("Diagnose & recover"))
                    .child(
                        Button::new("maintenance-close").label("Back").on_click(
                            cx.listener(|_, _, _, cx| cx.emit(MaintenanceViewEvent::Close)),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(tokens.spacing.xs)
                    .p(tokens.spacing.sm)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(MaintenanceTab::ALL.into_iter().map(|tab| {
                        Button::new(format!("maintenance-tab-{}", tab.id()))
                            .label(tab.title())
                            .selected(self.selected_tab == tab)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected_tab = tab;
                                this.error = None;
                                this.receipt = None;
                                cx.notify();
                            }))
                    })),
            )
            .child(
                div()
                    .id("maintenance-content")
                    .role(Role::Region)
                    .aria_label(self.selected_tab.title())
                    .flex_1()
                    .min_h_0()
                    .p(tokens.spacing.lg)
                    .child(content),
            )
            .when_some(self.busy.clone(), |panel, busy| {
                panel.child(status(busy, false, cx))
            })
            .when_some(self.error.clone(), |panel, error| {
                panel.child(status(error, true, cx))
            })
            .when_some(self.receipt.clone(), |panel, receipt| {
                panel.child(status(receipt, false, cx))
            })
    }
}

fn set_error(view: &mut MaintenanceView, message: String) {
    view.error = Some(message);
}

fn status(message: String, danger: bool, cx: &mut Context<MaintenanceView>) -> impl IntoElement {
    let tokens = cx.theme().semantic_tokens();
    div()
        .id("maintenance-status")
        .role(Role::Status)
        .aria_label(if danger {
            "Maintenance error"
        } else {
            "Maintenance status"
        })
        .w_full()
        .px(tokens.spacing.md)
        .py(tokens.spacing.sm)
        .border_t_1()
        .border_color(cx.theme().border)
        .when(danger, |bar| bar.text_color(cx.theme().danger))
        .child(message)
}
