//! MockTransport and the fixed method/path table of [`KnowledgeOperation`].

use std::sync::Arc;
use std::time::Duration;

use agent24_knowledge::{
    KnowledgeOperation, KnowledgeTransport, MockTransport, RecordedRequest, TransportError,
    TransportResponse,
};

#[tokio::test]
async fn operation_method_path_and_body_are_fixed() {
    assert_eq!(
        KnowledgeOperation::ALL,
        [
            KnowledgeOperation::Capabilities,
            KnowledgeOperation::Ready,
            KnowledgeOperation::Search,
            KnowledgeOperation::Read,
        ]
    );
    assert_eq!(KnowledgeOperation::ALL.len(), 4);
    assert_ne!(KnowledgeOperation::ALL[0], KnowledgeOperation::ALL[1]);
    assert_ne!(KnowledgeOperation::ALL[0], KnowledgeOperation::ALL[2]);
    assert_ne!(KnowledgeOperation::ALL[0], KnowledgeOperation::ALL[3]);
    assert_ne!(KnowledgeOperation::ALL[1], KnowledgeOperation::ALL[2]);
    assert_ne!(KnowledgeOperation::ALL[1], KnowledgeOperation::ALL[3]);
    assert_ne!(KnowledgeOperation::ALL[2], KnowledgeOperation::ALL[3]);

    for op in KnowledgeOperation::ALL {
        match op {
            KnowledgeOperation::Capabilities => {
                assert_eq!(op.method(), "GET");
                assert_eq!(op.path(), "/system/capabilities");
                assert!(!op.has_body());
            }
            KnowledgeOperation::Ready => {
                assert_eq!(op.method(), "POST");
                assert_eq!(op.path(), "/agent24/knowledge/ready");
                assert!(op.has_body());
            }
            KnowledgeOperation::Search => {
                assert_eq!(op.method(), "POST");
                assert_eq!(op.path(), "/agent24/knowledge/search");
                assert!(op.has_body());
            }
            KnowledgeOperation::Read => {
                assert_eq!(op.method(), "POST");
                assert_eq!(op.path(), "/agent24/knowledge/read");
                assert!(op.has_body());
            }
        }
    }
}

#[tokio::test]
async fn dyn_box_and_arc_in_spawn() {
    let boxed_mock = MockTransport::new();
    boxed_mock.push(
        KnowledgeOperation::Search,
        Ok(TransportResponse {
            status: 200,
            body: b"from-box".to_vec(),
        }),
    );
    let boxed: Box<dyn KnowledgeTransport> = Box::new(boxed_mock);
    assert_eq!(
        boxed
            .send(
                KnowledgeOperation::Search,
                b"{\"q\":1}".to_vec(),
                Duration::from_millis(25),
            )
            .await,
        Ok(TransportResponse {
            status: 200,
            body: b"from-box".to_vec(),
        })
    );

    let arc_mock = Arc::new(MockTransport::new());
    arc_mock.push(
        KnowledgeOperation::Read,
        Ok(TransportResponse {
            status: 200,
            body: b"from-arc".to_vec(),
        }),
    );
    let transport: Arc<dyn KnowledgeTransport> = arc_mock;
    let joined = tokio::spawn(async move {
        transport
            .send(
                KnowledgeOperation::Read,
                b"{\"id\":2}".to_vec(),
                Duration::from_millis(30),
            )
            .await
    })
    .await;
    let Ok(got) = joined else {
        panic!("spawned task panicked");
    };
    assert_eq!(
        got,
        Ok(TransportResponse {
            status: 200,
            body: b"from-arc".to_vec(),
        })
    );
}

#[tokio::test]
async fn queues_are_fifo_and_do_not_cross_operations() {
    let mock = MockTransport::new();
    let deadline = Duration::from_millis(40);
    mock.push(
        KnowledgeOperation::Search,
        Ok(TransportResponse {
            status: 200,
            body: b"search-1".to_vec(),
        }),
    );
    mock.push(
        KnowledgeOperation::Search,
        Ok(TransportResponse {
            status: 200,
            body: b"search-2".to_vec(),
        }),
    );
    assert_eq!(
        mock.send(KnowledgeOperation::Read, b"{}".to_vec(), deadline)
            .await,
        Err(TransportError::Other("mock: no scripted reply"))
    );
    assert_eq!(
        mock.send(KnowledgeOperation::Search, b"{\"n\":1}".to_vec(), deadline,)
            .await,
        Ok(TransportResponse {
            status: 200,
            body: b"search-1".to_vec(),
        })
    );
    assert_eq!(
        mock.send(KnowledgeOperation::Search, b"{\"n\":2}".to_vec(), deadline,)
            .await,
        Ok(TransportResponse {
            status: 200,
            body: b"search-2".to_vec(),
        })
    );
}

#[tokio::test]
async fn non_2xx_and_scripted_timeout_pass_through() {
    let mock = MockTransport::new();
    let deadline = Duration::from_millis(7);
    let envelope = br#"{"code":401,"msg":"denied"}"#.to_vec();
    mock.push(
        KnowledgeOperation::Ready,
        Ok(TransportResponse {
            status: 401,
            body: envelope.clone(),
        }),
    );
    mock.push(KnowledgeOperation::Ready, Err(TransportError::Timeout));
    assert_eq!(
        mock.send(KnowledgeOperation::Ready, b"{}".to_vec(), deadline)
            .await,
        Ok(TransportResponse {
            status: 401,
            body: envelope,
        })
    );
    assert_eq!(
        mock.send(KnowledgeOperation::Ready, b"{}".to_vec(), deadline)
            .await,
        Err(TransportError::Timeout)
    );
}

#[tokio::test]
async fn empty_queue_returns_no_scripted_reply() {
    let mock = MockTransport::new();
    assert_eq!(
        mock.send(
            KnowledgeOperation::Ready,
            Vec::new(),
            Duration::from_millis(5),
        )
        .await,
        Err(TransportError::Other("mock: no scripted reply"))
    );
}

#[tokio::test]
async fn body_on_get_is_rejected_and_leaves_the_queue() {
    let mock = MockTransport::new();
    let deadline = Duration::from_millis(15);
    mock.push(
        KnowledgeOperation::Capabilities,
        Ok(TransportResponse {
            status: 204,
            body: b"still-queued".to_vec(),
        }),
    );
    assert_eq!(
        mock.send(KnowledgeOperation::Capabilities, b"{}".to_vec(), deadline,)
            .await,
        Err(TransportError::Other("mock: body on a request without one"))
    );
    assert_eq!(
        mock.send(KnowledgeOperation::Capabilities, Vec::new(), deadline)
            .await,
        Ok(TransportResponse {
            status: 204,
            body: b"still-queued".to_vec(),
        })
    );
}

#[tokio::test]
async fn requests_are_recorded_in_call_order_including_rejected() {
    let mock = MockTransport::new();
    let d_search = Duration::from_millis(11);
    let d_rejected = Duration::from_millis(22);
    let d_caps = Duration::from_secs(3);
    mock.push(
        KnowledgeOperation::Capabilities,
        Ok(TransportResponse {
            status: 200,
            body: b"caps".to_vec(),
        }),
    );
    assert_eq!(
        mock.send(KnowledgeOperation::Search, b"abc".to_vec(), d_search)
            .await,
        Err(TransportError::Other("mock: no scripted reply"))
    );
    assert_eq!(
        mock.send(KnowledgeOperation::Capabilities, b"zz".to_vec(), d_rejected,)
            .await,
        Err(TransportError::Other("mock: body on a request without one"))
    );
    assert_eq!(
        mock.send(KnowledgeOperation::Capabilities, Vec::new(), d_caps)
            .await,
        Ok(TransportResponse {
            status: 200,
            body: b"caps".to_vec(),
        })
    );
    assert_eq!(
        mock.requests(),
        vec![
            RecordedRequest {
                op: KnowledgeOperation::Search,
                body: b"abc".to_vec(),
                deadline: d_search,
            },
            RecordedRequest {
                op: KnowledgeOperation::Capabilities,
                body: b"zz".to_vec(),
                deadline: d_rejected,
            },
            RecordedRequest {
                op: KnowledgeOperation::Capabilities,
                body: Vec::new(),
                deadline: d_caps,
            },
        ]
    );
}

#[tokio::test]
async fn transport_error_display_has_no_url_or_body() {
    let timeout = TransportError::Timeout.to_string();
    let unreachable = TransportError::Unreachable.to_string();
    let too_large = TransportError::ResponseTooLarge.to_string();
    let other = TransportError::Other("x").to_string();
    assert_eq!(timeout, "knowledge transport: deadline exceeded");
    assert_eq!(unreachable, "knowledge transport: server unreachable");
    assert_eq!(too_large, "knowledge transport: response too large");
    assert_eq!(other, "knowledge transport: x");
    for text in [&timeout, &unreachable, &too_large, &other] {
        assert!(!text.contains("http://"));
        assert!(!text.contains("https://"));
        assert!(!text.contains("/system/capabilities"));
        assert!(!text.contains("/agent24/knowledge/search"));
        assert!(!text.contains("{\"code\":401}"));
    }
}
