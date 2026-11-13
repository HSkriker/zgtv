//! HTTP 服务主体（hyper 1.x）—— 每连接一个 tokio task（等价 ThreadingHTTPServer 的 daemon 线程）。
//! 通用 handler：8786 与 8787 各自注入各自的 dispatch。

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request as HReq, Response as HResp, StatusCode};
use hyper_util::rt::TokioIo;

use crate::http::{parse_qs, Req, Resp};

/// 请求体上限：全部接口都是小 JSON，2MB 足够，同时挡住超大 body 打爆内存。
const MAX_BODY: usize = 2 * 1024 * 1024;

/// 通用请求处理器（同步函数；在阻塞线程池中执行）。
pub type Handler = Arc<dyn Fn(&Req) -> Resp + Send + Sync>;

/// 阻塞运行（内部自建 tokio runtime），返回 = shutdown 置位。
pub fn serve(port: u16, handler: Handler, shutdown: Arc<AtomicBool>) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|_| "端口被占用，请关闭整合版或使用 --port 指定其他端口。".to_string())?;
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _addr) = match accepted {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let handler = Arc::clone(&handler);
                    tokio::spawn(async move {
                        let svc = service_fn(move |req: HReq<Incoming>| {
                            let handler = Arc::clone(&handler);
                            async move { handle(handler, req).await }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), svc)
                            .await;
                    });
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
        }
        Ok(())
    })
}

async fn handle(handler: Handler, req: HReq<Incoming>) -> Result<HResp<Full<Bytes>>, Infallible> {
    let method = req.method().as_str().to_string();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query = parse_qs(uri.query().unwrap_or(""));
    let mut headers: HashMap<String, String> = HashMap::new();
    for (k, v) in req.headers() {
        headers.insert(k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string());
    }
    let body = match Limited::new(req.into_body(), MAX_BODY).collect().await {
        Ok(b) => b.to_bytes().to_vec(),
        Err(_) => {
            return Ok(resp_to_hyper(Resp::json(
                &serde_json::json!({"error": "请求体过大或读取失败"}),
                413,
            )))
        }
    };
    let req = Req { method, path, query, headers, body };

    // zg-core/zg-market 是同步 IO：放到阻塞线程池，避免卡住 reactor
    let result = tokio::task::spawn_blocking(move || handler(&req))
        .await
        .unwrap_or_else(|_| Resp::json(&serde_json::json!({"error": "internal error"}), 500));

    Ok(resp_to_hyper(result))
}

fn resp_to_hyper(result: Resp) -> HResp<Full<Bytes>> {
    let mut builder = HResp::builder()
        .status(StatusCode::from_u16(result.status).unwrap_or(StatusCode::OK));
    for (k, v) in &result.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    builder
        .body(Full::new(result.body))
        .unwrap_or_else(|_| HResp::new(Full::new(Bytes::new())))
}
