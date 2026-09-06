use super::*;

#[tokio::test]
async fn verifier_services_exact_cancel_and_shutdown_but_never_detaches_worker() {
    for shutdown in [false, true] {
        let operation = OperationId::new();
        let token = tokio_util::sync::CancellationToken::new();
        let worker_token = token.clone();
        let joined = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_joined = joined.clone();
        let worker = tokio::spawn(async move {
            worker_token.cancelled().await;
            worker_joined.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(CompletionEvidence::new(
                operation,
                crate::completion_evidence::WorkKind::Root,
                crate::completion_evidence::EvidenceOwner::Native,
                CompletionClaim::Cancelled,
                Default::default(),
            )
            .unwrap())
        });
        let (sender, mut receiver) = mpsc::channel(4);
        sender
            .send(RuntimeCommand::InterruptOperation {
                operation_id: OperationId::new(),
            })
            .await
            .unwrap();
        sender
            .send(if shutdown {
                RuntimeCommand::Shutdown
            } else {
                RuntimeCommand::InterruptOperation {
                    operation_id: operation,
                }
            })
            .await
            .unwrap();
        let mut rejected = 0;
        let (result, stopped) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            await_verifier(&mut receiver, operation, &token, worker, || rejected += 1),
        )
        .await
        .unwrap();
        assert!(result.unwrap().is_ok());
        assert_eq!(stopped, shutdown);
        assert_eq!(rejected, 1);
        assert!(token.is_cancelled());
        assert!(joined.load(std::sync::atomic::Ordering::SeqCst));
    }
}
