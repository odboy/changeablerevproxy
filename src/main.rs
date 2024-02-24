//! 方案一：loop循环读写。
//! 参考代码： https://gist.github.com/fragsalat/d55cc7a88ed56a97b89c2e13362a58a8
//!
//! 方案二：io:copy (tokio::io::AsyncReadExt, tokio::io::AsyncWriteExt)
//! 参考： https://anirudhsingh.dev/blog/2021/07/writing-a-tcp-proxy-in-rust/
//!
//! 方案三：滑动窗口的缓冲区  Cursor？

use std::net::{SocketAddr};
use std::sync::OnceLock;
use std::time::Duration;

use axum::{Form, Router};
use axum::extract::ConnectInfo;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum_extra::{headers, TypedHeader};
use chrono::Local;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio::try_join;

struct AppData {
    portal_port: u16,
    listen_port: u16,
    direct_port: u16,
    rps_handler: JoinHandle<()>,
}

static APP_DATA: OnceLock<Mutex<AppData>> = OnceLock::new();

// pub static LISTEN_PORT: OnceLock<Mutex<u16>> = OnceLock::new();
// pub static RPS_HANDLER: OnceLock<Mutex<JoinHandle<()>>> = OnceLock::new();

#[derive(serde::Serialize, serde::Deserialize)]
struct CERT {
    pwd: String,
}

#[derive(Debug, Clone, clap::Parser, Serialize, Deserialize, Default)]
#[clap(author, version, about, long_about = None)]
pub struct Config {
    /// 服务监听端口
    #[clap(short, long, default_value_t = 9669)]
    pub portal: u16,

    /// 目标端口
    #[clap(short, long)]
    pub target: u16,
}


async fn hello(user_agent: Option<TypedHeader<headers::UserAgent>>,
               ConnectInfo(addr): ConnectInfo<SocketAddr>) -> impl IntoResponse {
    println!("GET {} | {:?} | {}", Local::now().format("%Y-%m-%d %H:%M:%S"), addr, user_agent.and_then(|ua|Some(ua.to_string())).unwrap_or("".to_string()));

    let output = format!("<h2>{}</h2>\
<form action=\"#\" method=\"POST\">
<input type=\"text\" name=\"pwd\"/>
<input type=\"submit\" value=\"修改\">
</form>", APP_DATA.get().unwrap().lock().await.listen_port);
    Html(output)
}

/// 有问题，这里不应该是http反代，而是端口转发。
async fn revproxy_serve() {
    // 随机选择端口
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0")
        .await
        .unwrap();

    // 获取新分配的端口
    let new_port =
        match listener.local_addr() {
            Ok(local_addr) => {
                println!("reverse proxy port: {}", local_addr.port());
                local_addr.port()
            }
            Err(e) => {
                println!("{}", e);
                0
            }
        };
    // 更新端口到全局变量。 同时获取目标端口。
    let mut direct_port = 0u16;
    match APP_DATA.get() {
        Some(mutex_port) => {
            let mut port = mutex_port.lock().await;
            port.listen_port = new_port;
            direct_port = port.direct_port;
        }
        None => {
            println!("OnceLock获取异常...");
        }
    }

    loop {
        // 持续监听连接请求
        match listener.accept().await {
            Ok((mut incoming,socket_addr)) => {

                // if let Err(err) = tcp_echo(&mut incoming).await {
                //     println!("{}",err);
                // }

                match tokio::net::TcpStream::connect(format!("127.0.0.1:{}",direct_port)).await {
                    Ok(mut outgoing) => {
                        println!("{}\t{}-{} => {}-{}",
                            Local::now().format("%Y-%m-%d %H:%M:%S"),
                            incoming.peer_addr().and_then(|a|Ok(a.to_string())).unwrap_or("".to_string()),
                            incoming.local_addr().and_then(|a|Ok(a.to_string())).unwrap_or("".to_string()),
                            outgoing.local_addr().and_then(|a|Ok(a.to_string())).unwrap_or("".to_string()),
                            outgoing.peer_addr().and_then(|a|Ok(a.to_string())).unwrap_or("".to_string())
                        );

                        // tokio::spawn(data_forwarding_bidirection(incoming, ongoing));
                        tokio::spawn(data_forwarding_bidirection_2(incoming, outgoing));
                        // tokio::spawn(data_forwarding(&mut (incoming.), &mut ongoing));
                        // tokio::spawn(data_forwarding(&mut ongoing, &mut incoming));
                    }
                    Err(_) => {
                        println!("连接127.0.0.1:{}失败",direct_port);
                        let _ = incoming.shutdown().await;
                    }
                }
            }
            Err(e) => {

            }
        }
    }

}

async fn tcp_echo(stream: &mut TcpStream) ->anyhow::Result<(),anyhow::Error>{
    // let mut buffer = Vec::with_capacity(1024);
    let mut buffer = [0; 1024];
    loop {
        match stream.read(&mut buffer).await{
            Ok(bytes_read)=> {
                println!("Receive {} bytes", bytes_read);
                if bytes_read == 0 {
                    stream.shutdown().await?;
                    break;
                }
                match stream.write(&buffer[..bytes_read]).await {
                    Ok(bytes_write) => {
                        stream.flush().await?;
                        println!("Send {} bytes", bytes_read);
                    }
                    Err(err) => {
                        println!("Send Back Error: {}",err);
                        stream.shutdown().await?;
                        break;
                    }
                }
            }
            Err(err) => {
                println!("{}",err);
            }
        }
    }
    Ok(())
}

async fn data_forwarding(incoming: &mut TcpStream, outgoing: &mut TcpStream) -> anyhow::Result<(), anyhow::Error> {
    let mut buffer = [0; 1024 * 8];   // 8K 缓冲区

    loop {
        match incoming.read(&mut buffer).await {
            Ok(bytes_read) => {
                if bytes_read == 0 {
                    incoming.shutdown().await?;
                    outgoing.shutdown().await?;
                    break;
                }
                match outgoing.write(&buffer[..bytes_read]).await {
                    Ok(bytes_write) => {
                        outgoing.flush().await?;
                    }
                    Err(err) => {
                        incoming.shutdown().await?;
                        outgoing.shutdown().await?;
                        break;
                    }
                }
            }
            Err(err) => {
                break;
            }
        }
    }
    Ok(())
}

/// 使用io::copy直接交换数据
async fn data_forwarding_bidirection_2(mut downstream: TcpStream, mut upstream: TcpStream) -> anyhow::Result<(), anyhow::Error> {

    let (mut downstream_read, mut downstream_write) = downstream.split();
    let (mut upstream_read, mut upstream_write) = upstream.split();

    let handle_one = async {
        // tokio::io::copy 本质应该是poll_read和poll_write。
        tokio::io::copy(&mut downstream_read, &mut upstream_write).await
    };

    let handle_two = async {
        tokio::io::copy(&mut upstream_read, &mut downstream_write).await
    };
    match try_join!(handle_one, handle_two) {
        Ok(a) => {
            let _ = downstream.shutdown().await;
            let _ = upstream.shutdown().await;
            Ok(())
        }
        Err(err) => {
            let _ = downstream.shutdown().await;
            let _ = upstream.shutdown().await;
            Ok(())
        }
    }
}


/// 低效率。 可能是因为read和write非同步执行所致。
async fn data_forwarding_bidirection(mut downstream: TcpStream, mut upstream: TcpStream) -> anyhow::Result<(), anyhow::Error> {
    let mut buffer1 = [0; 1024 * 4];
    let mut buffer2 = [0; 1024 * 4];

    let downstream_addr = downstream.local_addr().and_then(|a|Ok(a.to_string())).unwrap_or("".to_string());
    let upstream_addr = upstream.peer_addr().and_then(|a|Ok(a.to_string())).unwrap_or("".to_string());

    loop {
        tokio::select! {
            Ok(bytes_read) = upstream.read(&mut buffer2) => {
                if cfg!(debug_assertions) {
                    println!("{} -> {} : read {} bytes",
                        upstream_addr,
                        downstream_addr,
                        bytes_read
                    );
                }
                if bytes_read == 0 {
                    let _ = downstream.shutdown().await;
                    let _ = upstream.shutdown().await;
                    break;
                }
                let instant = Instant::now();
                match downstream.write(&buffer2[..bytes_read]).await {
                    Ok(bytes_write) => {
                        if cfg!(debug_assertions) {
                            println!("{} -> {} : write {} bytes {:?}",
                                upstream_addr,
                                downstream_addr,
                                bytes_write,instant.elapsed());
                        }
                        // let _ = incoming.flush().await;
                    }
                    Err(err) => {
                        let _ = downstream.shutdown().await;
                        let _ = upstream.shutdown().await;
                        break;
                    }
                }
            }

            Ok(bytes_read) = downstream.read(&mut buffer1) => {
                if cfg!(debug_assertions) {
                    println!("{} -> {} : read {} bytes",
                        downstream_addr,
                        upstream_addr,
                        bytes_read);
                }
                if bytes_read == 0 {
                    let _ = downstream.shutdown().await;
                    let _ = upstream.shutdown().await;
                    break;
                }
                let instant = Instant::now();
                match upstream.write(&buffer1[..bytes_read]).await {
                    Ok(bytes_write) => {
                        if cfg!(debug_assertions) {
                            println!("{} -> {} : write {} bytes {:?}",
                                downstream_addr,
                                upstream_addr,
                                bytes_write,instant.elapsed());
                        }
                        // let _ = outgoing.flush().await;
                    }
                    Err(err) => {
                        let _ = downstream.shutdown().await;
                        let _ = upstream.shutdown().await;
                        break;
                    }
                }
            }

            else => {
                if cfg!(debug_assertions){
                    println!("else branch...");
                }
                let _ = downstream.shutdown().await;
                let _ = upstream.shutdown().await;
            }
        }
    }

    // loop {
    //     tokio::select! {
    //         _ = incoming.readable() => {
    //             match incoming.read(&mut buffer1).await{
    //                         Ok(bytes_read)=> {
    //                             if bytes_read == 0 {
    //                                 outgoing.shutdown().await?;
    //                                 break;
    //                             }
    //                             match outgoing.write(&buffer1[..bytes_read]).await {
    //                                 Ok(bytes_write) => {
    //                                     outgoing.flush().await?;
    //                                 }
    //                                 Err(err) => {
    //                                     outgoing.shutdown().await?;
    //                                     break;
    //                                 }
    //                             }
    //                         }
    //                         Err(err) => {
    //                             break;
    //                         }
    //                     }
    //         }
    //
    //         _ = outgoing.readable() => {
    //             match outgoing.read(&mut buffer2).await{
    //                         Ok(bytes_read)=> {
    //                             if bytes_read == 0 {
    //                                 incoming.shutdown().await?;
    //                                 break;
    //                             }
    //                             match incoming.write(&buffer2[..bytes_read]).await {
    //                                 Ok(bytes_write) => {
    //                                     incoming.flush().await?;
    //                                 }
    //                                 Err(err) => {
    //                                     incoming.shutdown().await?;
    //                                     break;
    //                                 }
    //                             }
    //                         }
    //                         Err(err) => {
    //                             break;
    //                         }
    //                     }
    //         }
    //     }
    // }
    if cfg!(debug_assertions) {
        println!("连接断开");
    }
    Ok(())
}


async fn changeport(user_agent: Option<TypedHeader<headers::UserAgent>>,
                    ConnectInfo(addr): ConnectInfo<SocketAddr>, Form(pwd): Form<CERT>) -> Response {
    println!("POST {} | {:?} | {}", Local::now().format("%Y-%m-%d %H:%M:%S"), addr, user_agent.and_then(|ua|Some(ua.to_string())).unwrap_or("".to_string()));

    if pwd.pwd == Local::now().format("AA%Y%m%d").to_string() {
        println!("{}\t修改监听端口",pwd.pwd);
    } else {
        return "WRONG".into_response()
    };
    // 杀死原有task,重新运行新的task。
    match APP_DATA.get() {
        Some(app_data) => {
            let mut data = app_data.lock().await;
            data.listen_port = 0;
            data.rps_handler.abort_handle().abort();
            let rps_handler = tokio::spawn(revproxy_serve());
            data.rps_handler = rps_handler;
        }
        None => {
        }
    };

    // 等待新端口生效~
    let mut new_port = 0u16;
    loop {
        if let Some(app_data) = APP_DATA.get() {
            let mut data = app_data.lock().await;
            new_port = data.listen_port
        }
        if new_port == 0 {
            let _ = tokio::time::sleep(Duration::from_millis(100)).await;
        } else {
            break
        }
    }

    match APP_DATA.get() {
        Some(app_data) => {
            let data = app_data.lock().await;
            Html(format!("<h2>{}</h2>\
<form action=\"#\" method=\"POST\">
<input type=\"text\" name=\"pwd\"/>
<input type=\"submit\" value=\"修改\">
</form>",data.listen_port)).into_response()
        }
        None => {
            Html("Done.").into_response()
        }
    }
}

async fn portal_serve(portal_port: u16) {
    let app = Router::new().route(
        "/_changeport",
        get(hello).post(changeport));
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", portal_port))
        .await
        .unwrap();

    // todo:: 尝试获取当前公网IP。
    println!("portal: http://0.0.0.0:{}/_changeport", portal_port);
    axum::serve(listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
    ).await.unwrap();
}

#[tokio::main]
async fn main() {
    let config = Config::parse();

    // 在新线程中启动反代服务
    let rps_handler = tokio::spawn(revproxy_serve());
    let mut app_data = APP_DATA.get_or_init(|| Mutex::new(AppData {
        portal_port: config.portal,
        listen_port: 0,
        direct_port: config.target,
        rps_handler: rps_handler,
    }));

    // 启动门户服务
    portal_serve(config.portal).await;
}