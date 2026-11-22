//! The MCP HTTP transport: serving the protocol on a TCP port.
//!
//! The server thread owns a tokio runtime of its own and answers HTTP/1.x
//! through hyper, handing rmcp's streamable-HTTP service every request. The
//! commands it receives are posted to a sink, the same contract the stdio
//! server follows, so a world that is not sendable still runs every tool on
//! its own thread.
//!
//! [serve_http_with] returns once the port is bound, with the handle that
//! names the address and stops the server again.

use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};

use crate::mcp::{McpCommand, McpServer};

/// Serves MCP over streamable HTTP on addr, handing every command to sink.
///
/// The sink is called on a thread of the server's own as each command
/// arrives; returning false from it ends the pump, which is how a caller
/// says the inbox it feeds is gone. The host answers the list_worlds tool
/// itself: when a command's [McpCommand::wants_worlds] is true, it names
/// every world it carries through [McpCommand::answer_worlds] instead of
/// dispatching a job, exactly as the stdio server expects.
///
/// The call returns once the port is bound, so a caller that asked for port
/// 0 can name the address it was given; the server keeps running on a
/// thread of its own until the returned [McpHttpServer] is stopped or
/// dropped.
///
/// # Errors
///
/// Fails when the tokio runtime cannot be built, the thread cannot start, or
/// the port cannot be bound.
pub fn serve_http_with(
    sink: impl Fn(McpCommand) -> bool + Send + 'static,
    addr: SocketAddr,
) -> Result<McpHttpServer, Box<dyn Error + Send + Sync>> {
    // Every driver, not just the timer: the transport reads and writes
    // sockets, which panics on a runtime without the IO driver.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let (bound, bound_rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("unlit_ecs-mcp-http".to_owned())
        .spawn(move || {
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(addr).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = bound.send(Err(error));
                        return;
                    }
                };
                let local_addr = match listener.local_addr() {
                    Ok(local_addr) => local_addr,
                    Err(error) => {
                        let _ = bound.send(Err(error));
                        return;
                    }
                };
                let _ = bound.send(Ok(local_addr));
                let config = match allowed_hosts(local_addr) {
                    Some(hosts) => StreamableHttpServerConfig::default().with_allowed_hosts(hosts),
                    None => StreamableHttpServerConfig::default().disable_allowed_hosts(),
                };
                // The same pump the stdio server runs: a bounded inbox keeps a
                // slow host from collecting an unbounded backlog, and the pump
                // ends once the service is gone and the last sender with it.
                let (sender, receiver) = async_channel::bounded::<McpCommand>(4);
                tokio::task::spawn_blocking(move || {
                    while let Ok(command) = receiver.recv_blocking() {
                        if !sink(command) {
                            break;
                        }
                    }
                });
                // The factory keeps the only sender, so every session builds
                // its server against the one inbox; when the service drops,
                // the channel closes and the pump above ends with it.
                let service = StreamableHttpService::new(
                    move || Ok(McpServer::new(sender.clone())),
                    Arc::new(LocalSessionManager::default()),
                    config,
                );
                let mut stop_rx = stop_rx;
                loop {
                    tokio::select! {
                        // A dropped watch sender fires the same branch, so
                        // dropping the handle stops the server like an
                        // explicit stop does.
                        _ = stop_rx.changed() => break,
                        accepted = listener.accept() => match accepted {
                            Ok((stream, _)) => {
                                // One connection, one task: a session's
                                // requests arrive in order on the keep-alive
                                // connection, which is all it asks for.
                                let http = http1::Builder::new();
                                let service = service.clone();
                                tokio::spawn(async move {
                                    let _ = http
                                        .serve_connection(
                                            TokioIo::new(stream),
                                            TowerToHyperService::new(service),
                                        )
                                        .await;
                                });
                            }
                            // A failed accept ends the server: the socket is
                            // broken, so there is nothing left to serve.
                            Err(error) => {
                                log::error!("the MCP HTTP server could not accept a connection: {error}");
                                break;
                            },
                        },
                    }
                }
            });
        })?;
    let local_addr = match bound_rx.recv() {
        Ok(Ok(local_addr)) => local_addr,
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => return Err("the MCP HTTP server thread stopped before binding".into()),
    };
    Ok(McpHttpServer {
        local_addr,
        stop,
        thread: Some(thread),
    })
}

/// The `Host` headers a client may reach a server bound to `addr` through, or
/// `None` when the bound address cannot be named.
///
/// The transport rejects a request whose `Host` names anything else, which is
/// its DNS-rebinding guard: a page that resolves its own domain to this port
/// would otherwise drive the host. Its default list is the loopback names
/// alone, so a server bound to a routable address refuses every request —
/// including the ones from the client that asked for that address. The list
/// therefore holds the names this server actually answers to: the bound
/// address itself, plus the loopback names it may also be reached under.
///
/// A wildcard bind (`0.0.0.0` or `[::]`) has no one name to allow — the client
/// arrives under whichever name the machine has — so there is nothing to check
/// the header against and the guard is switched off, with a warning. A caller
/// that needs the guard on a routable interface binds the interface's address.
fn allowed_hosts(addr: SocketAddr) -> Option<Vec<String>> {
    if addr.ip().is_unspecified() {
        log::warn!(
            "the MCP HTTP server is bound to {addr}, which has no single name to \
             check the Host header against, so its DNS-rebinding guard is off; \
             bind a specific address to keep it on"
        );
        return None;
    }
    let mut hosts = vec![addr.to_string()];
    let loopback = if addr.is_ipv6() {
        ["localhost", "::1"]
    } else {
        ["localhost", "127.0.0.1"]
    };
    hosts.extend(loopback.iter().map(|host| (*host).to_owned()));
    Some(hosts)
}

/// A running MCP HTTP server, kept alive by the handle itself.
///
/// Dropping the handle stops the server: the accept loop watches the channel
/// the handle's sender lives in, and a dropped sender closes it. Stopping
/// through [McpHttpServer::stop] also joins the server thread, so a caller
/// can wait until the port is free again.
pub struct McpHttpServer {
    local_addr: SocketAddr,
    stop: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl McpHttpServer {
    /// The address the server answers on: the port a caller asking for port
    /// 0 was given.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The base URL a client points its transport at.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}/", self.local_addr)
    }

    /// Stops the server and waits for its thread to finish, so the port is
    /// free again when this returns.
    pub fn stop(mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// The tests bind loopback TCP ports, and Miri's isolation rejects sockets; the
// stdio tests cover the same sink and dispatch path behind the transport.
#[cfg(all(test, not(miri)))]
mod tests {
    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;
    use rmcp::transport::StreamableHttpClientTransport;

    use super::{allowed_hosts, serve_http_with};
    use crate::mcp::host::{McpCommand, WorldEntry, WorldRef};

    /// The host a test answers with: every list_worlds names one world, so a
    /// client reads the answer straight off the transport.
    fn sink(command: McpCommand) -> bool {
        command.answer_worlds(vec![WorldEntry {
            world: WorldRef::Host,
            name: "test",
        }]);
        true
    }

    #[tokio::test]
    async fn a_client_reaches_the_host_over_http() {
        let server = serve_http_with(sink, "127.0.0.1:0".parse().expect("the loopback parses"))
            .expect("the server starts");
        let transport = StreamableHttpClientTransport::from_uri(format!("{}mcp", server.url()));
        let client = ().serve(transport).await.expect("the client serves");

        let tools = client.list_tools(None).await.expect("the tools list");
        assert!(
            tools
                .tools
                .iter()
                .any(|tool| tool.name.as_ref() == "list_worlds")
        );

        let result = client
            .call_tool(CallToolRequestParams::new("list_worlds"))
            .await
            .expect("the tool answers");
        let value = result
            .structured_content
            .expect("the world list is structured");
        assert!(value.to_string().contains("test"));

        client.cancel().await.expect("the client leaves");
        server.stop();
    }

    /// A server bound to a routable address answers to that address, which
    /// the transport's loopback-only default would refuse.
    #[test]
    fn a_bound_address_is_one_of_the_names_it_answers_to() {
        let addr = "192.168.1.5:8080".parse().expect("the address parses");
        let hosts = allowed_hosts(addr).expect("a routable address has a name");
        assert!(hosts.contains(&String::from("192.168.1.5:8080")));
        assert!(hosts.contains(&String::from("127.0.0.1")));
        assert!(hosts.contains(&String::from("localhost")));
    }

    /// A wildcard bind has no name a client would arrive under, so there is
    /// nothing to compare the header against.
    #[test]
    fn a_wildcard_bind_has_no_name_to_check() {
        assert!(allowed_hosts("0.0.0.0:8080".parse().expect("the address parses")).is_none());
        assert!(allowed_hosts("[::]:8080".parse().expect("the address parses")).is_none());
    }

    /// Reaching a server under an address other than the loopback names it is
    /// bound to works: the guard admits the address, not just `localhost`.
    // macOS only binds 127.0.0.1 out of the loopback block, so the alias this
    // test uses cannot be bound there. The Host-guard rule it exercises is
    // covered by a_bound_address_is_one_of_the_names_it_answers_to on every
    // platform.
    #[cfg_attr(
        target_os = "macos",
        ignore = "macOS does not bind 127.0.0.2 loopback aliases"
    )]
    #[tokio::test]
    async fn a_client_reaches_a_server_bound_to_another_loopback_address() {
        // 127.0.0.2 is on the loopback interface but is not one of the names
        // the transport allows by default, so this is the case a routable bind
        // hits.
        let server = serve_http_with(sink, "127.0.0.2:0".parse().expect("the address parses"))
            .expect("the server starts");
        let transport = StreamableHttpClientTransport::from_uri(format!("{}mcp", server.url()));
        let client = ().serve(transport).await.expect("the client serves");

        let tools = client.list_tools(None).await.expect("the tools list");
        assert!(
            tools
                .tools
                .iter()
                .any(|tool| tool.name.as_ref() == "list_worlds")
        );

        client.cancel().await.expect("the client leaves");
        server.stop();
    }

    #[tokio::test]
    async fn a_stopped_server_frees_its_port() {
        let server = serve_http_with(sink, "127.0.0.1:0".parse().expect("the loopback parses"))
            .expect("the server starts");
        let addr = server.local_addr();
        // The stop joins the thread, so once it returns the listener is gone
        // and the port is the OS's to hand out again.
        server.stop();
        tokio::net::TcpListener::bind(addr)
            .await
            .expect("the port is released");
    }
}
