// Synthetic executable for the process sampler, not storage/resource evidence.
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(
        args,
        [
            "storage::history::execution_tests::protected_execution_resource_probe",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1"
        ]
    );
    assert!(std::env::var_os("XANA_HISTORY_VERIFY_PROFILE_ONLY").is_none());
    println!("fixture_pid={}", std::process::id());
    if std::env::var("XANA_TEST_HISTORY_WORKER_MODE").as_deref() == Ok("hang") {
        std::thread::sleep(std::time::Duration::from_secs(30));
        panic!("sampler failed to terminate fixture");
    }
    std::thread::sleep(std::time::Duration::from_millis(250));
    let count = std::env::var("XANA_HISTORY_PROBE_MESSAGES").unwrap();
    println!("test {} ... fixture_directory=synthetic", args[0]);
    println!("production_fixture messages={count} generation_ms=15 journal_bytes=100000");
    for trial in 0..5 {
        println!(
            "protected_execution messages={count} trial={trial} open_us=10 resume_us=20 page_median_us=8 page_p95_us=20 retained_entries=128 execution_bytes=1024 initial_page_bytes=1024"
        );
    }
    println!("immutable_verification messages={count} elapsed_ms=10");
    println!("protected_backup messages={count} elapsed_ms=20");
    println!("protected_restore messages={count} elapsed_ms=30");
    println!(
        "ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s"
    );
}
