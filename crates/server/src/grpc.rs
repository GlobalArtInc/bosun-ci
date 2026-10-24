use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::Router;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::http::header::CONTENT_TYPE;
use axum::middleware::{self, Next};
use axum::response::Response;
use http_body::Frame;
use tonic::Status;
use tonic::service::Routes;

use crate::shutdown::Shutdown;

const GRPC: &str = "application/grpc";

pub(crate) fn routes(routes: Routes, shutdown: Shutdown) -> Router {
    closing(routes.into_axum_router(), shutdown)
}

pub(crate) fn is_grpc(request: &Request) -> bool {
    request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with(GRPC))
}

fn closing(routes: Router, shutdown: Shutdown) -> Router {
    routes.layer(middleware::from_fn_with_state(shutdown, close_on_shutdown))
}

async fn close_on_shutdown(
    State(shutdown): State<Shutdown>,
    request: Request,
    next: Next,
) -> Response {
    next.run(request).await.map(|body| {
        Body::new(Closing {
            inner: body,
            closed: Box::pin(shutdown.reason()),
            done: false,
        })
    })
}

struct Closing {
    inner: Body,
    closed: Pin<Box<dyn Future<Output = Arc<str>> + Send>>,
    done: bool,
}

impl HttpBody for Closing {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        if self.done {
            return Poll::Ready(None);
        }
        if let Poll::Ready(frame) = Pin::new(&mut self.inner).poll_frame(cx) {
            self.done = frame.is_none();
            return Poll::Ready(frame);
        }
        let Poll::Ready(reason) = self.closed.as_mut().poll(cx) else {
            return Poll::Pending;
        };
        self.done = true;
        Poll::Ready(Some(Ok(Frame::trailers(unavailable(&reason)))))
    }

    fn is_end_stream(&self) -> bool {
        self.done || self.inner.is_end_stream()
    }
}

fn unavailable(reason: &str) -> HeaderMap {
    let mut trailers = HeaderMap::new();
    let _ = Status::unavailable(reason).add_header(&mut trailers);
    trailers
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::future::poll_fn;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use axum::Router;
    use axum::body::{Body, Bytes, HttpBody};
    use axum::http::Request;
    use axum::http::header::CONTENT_TYPE;
    use axum::response::Response;
    use axum::routing::post;
    use http_body::Frame;
    use tonic::{Code, Status};
    use tower::ServiceExt;

    use super::{GRPC, closing};
    use crate::shutdown;

    const REASON: &str = "the server is shutting down: received SIGTERM";

    struct Stream {
        ready: Option<Bytes>,
    }

    impl HttpBody for Stream {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
            match self.ready.take() {
                Some(message) => Poll::Ready(Some(Ok(Frame::data(message)))),
                None => Poll::Pending,
            }
        }
    }

    fn grpc(body: Body) -> Response {
        Response::builder()
            .header(CONTENT_TYPE, GRPC)
            .body(body)
            .expect("a response")
    }

    async fn call(shutdown: shutdown::Shutdown) -> Body {
        let routes = Router::new().route(
            "/bosun.test.Stream/Watch",
            post(|| async {
                grpc(Body::new(Stream {
                    ready: Some(Bytes::from_static(b"first message")),
                }))
            }),
        );
        let response = closing(routes, shutdown)
            .oneshot(
                Request::post("/bosun.test.Stream/Watch")
                    .header(CONTENT_TYPE, GRPC)
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await
            .expect("a response");
        response.into_body()
    }

    async fn frame(body: &mut Body) -> Option<Frame<Bytes>> {
        poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx))
            .await
            .transpose()
            .expect("a body without errors")
    }

    #[tokio::test]
    async fn a_stream_in_progress_ends_with_unavailable_and_the_reason_on_shutdown() {
        let (closer, shutdown) = shutdown::channel();
        let mut body = call(shutdown).await;
        let first = frame(&mut body).await.expect("the first message");
        assert_eq!(
            first.into_data().ok(),
            Some(Bytes::from_static(b"first message"))
        );
        let waiting = tokio::time::timeout(Duration::from_millis(50), frame(&mut body)).await;
        assert!(waiting.is_err(), "the stream ended before the shutdown");

        closer.close(REASON.to_owned());
        let trailers = frame(&mut body)
            .await
            .expect("a last frame")
            .into_trailers()
            .expect("trailers");
        let status = Status::from_header_map(&trailers).expect("a gRPC status");
        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(status.message(), REASON);
        assert!(
            frame(&mut body).await.is_none(),
            "frames after the trailers"
        );
        assert!(body.is_end_stream());
    }

    #[tokio::test]
    async fn frames_ready_at_shutdown_still_arrive_and_a_finished_body_stays_unchanged() {
        let (closer, shutdown) = shutdown::channel();
        let mut streaming = call(shutdown.clone()).await;
        let routes = Router::new().route(
            "/bosun.test.Stream/Once",
            post(|| async { grpc(Body::empty()) }),
        );
        let mut finished = closing(routes, shutdown)
            .oneshot(
                Request::post("/bosun.test.Stream/Once")
                    .header(CONTENT_TYPE, GRPC)
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await
            .expect("a response")
            .into_body();

        closer.close(REASON.to_owned());
        let first = frame(&mut streaming).await.expect("the first message");
        assert_eq!(
            first.into_data().ok(),
            Some(Bytes::from_static(b"first message"))
        );
        let last = frame(&mut streaming).await.expect("a last frame");
        assert!(last.is_trailers());
        assert!(
            frame(&mut finished).await.is_none(),
            "a finished body grew a frame"
        );
    }
}
