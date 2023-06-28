#[macro_use]
extern crate tracing;

use std::net::{IpAddr, UdpSocket};
use std::thread;

use anyhow::Result;
use cam_loop::cam_loop;
use rouille::Server;
use rouille::{Request, Response};
use run_loop::{run_loop, Client};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::{Candidate, Rtc};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::{broadcast, mpsc};
use util::{select_host_ip, CERT_CRT, CERT_KEY};

mod cam_loop;
mod run_loop;

fn init_log() {
    use std::env;
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "webcam=info,str0m=info");
    }

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();
}

#[tokio::main]
async fn main() {
    init_log();

    // Figure out some public IP address, since Firefox will not accept 127.0.0.1 for WebRTC traffic.
    let host_ip = select_host_ip().expect("a local host ip");

    // Channel to send new Rtc clients from the sync webserver to the async run loop.
    // By using "unbounded" channel we get a sync send on the tx side.
    let (tx_client, mut rx_client) = unbounded_channel();

    // Broadcast from single producer camera capture loop to all connected clients.
    // _rx_cam must not be deallocated, or the sender side would fail when there are no
    // clients disconnected.
    let (tx_cam, _rx_cam) = broadcast::channel(16);

    // Back channel from clients to cam_loop to request new keyframes from the h264 encoder.
    let (tx_keyframe, rx_keyframe) = mpsc::channel(1);

    // This web server is a simple _sync_ webserver (rouile) running on its own thread. WebRTC
    // requires an https HTML page as starting point, and we don't want to complicate this example with
    // setting up TLS in an async server.
    thread::spawn(move || start_web_server(host_ip, tx_client));

    // The webcam capture loop is independent of the clients.
    tokio::spawn(cam_loop(tx_cam.clone(), rx_keyframe));

    // Loop to accept new Rtc clients.
    while let Some(start) = rx_client.recv().await {
        // As we move to async world, we change the sync UdpSocket to a tokio UdpSocket.
        let socket = tokio::net::UdpSocket::from_std(start.1).expect("tokio UdpSocket from std");

        let client = Client {
            rtc: start.0,
            socket,
            rx_cam: tx_cam.subscribe(),
            tx_keyframe: tx_keyframe.clone(),
        };

        tokio::spawn(run_loop(client));
    }
}

fn start_web_server(host_ip: IpAddr, tx_client: UnboundedSender<ClientStart>) {
    let server = Server::new_ssl(
        "0.0.0.0:3000",
        move |request| web_request(request, host_ip, tx_client.clone()),
        CERT_CRT.to_vec(),
        CERT_KEY.to_vec(),
    )
    .expect("starting the web server");

    let port = server.server_addr().port();
    info!("Connect a browser to https://{:?}:{:?}", host_ip, port);

    server.run();
}

// Handle a web request.
fn web_request(
    request: &Request,
    host_ip: IpAddr,
    tx_client: UnboundedSender<ClientStart>,
) -> Response {
    if request.method() == "GET" {
        return Response::html(include_str!("webcam.html"));
    }

    // Expected POST SDP Offers.
    let mut data = request.data().expect("body to be available");

    let offer: SdpOffer = serde_json::from_reader(&mut data).expect("serialized offer");

    let answer = match start_rtc(host_ip, tx_client, offer) {
        Ok(v) => v,
        Err(error) => {
            warn!("Failed to start RTC: {:?}", error);
            return Response::text("Failed to start RTC").with_status_code(500);
        }
    };

    let body = serde_json::to_vec(&answer).expect("answer to serialize");

    Response::from_data("application/json", body)
}

// Bootstrap a new Rtc instance.
fn start_rtc(
    host_ip: IpAddr,
    tx_client: UnboundedSender<ClientStart>,
    offer: SdpOffer,
) -> Result<SdpAnswer> {
    // Spin up a UDP socket for the RTC.
    let socket = UdpSocket::bind(format!("{host_ip}:0"))?;
    let addr = socket.local_addr()?;
    info!("Bound UDP port: {}", addr);

    let mut rtc = Rtc::builder()
        // Uncomment this to see statistics
        // .set_stats_interval(Some(Duration::from_secs(1)))
        .set_ice_lite(true)
        .build();

    // Add the shared UDP socket as a host candidate
    let candidate = Candidate::host(addr)?;
    rtc.add_local_candidate(candidate);

    // Create an SDP Answer.
    let answer = rtc.sdp_api().accept_offer(offer)?;

    // The Rtc instance is shipped off to the main run loop.
    tx_client.send(ClientStart(rtc, socket))?;

    Ok(answer)
}

#[derive(Debug)]
struct ClientStart(Rtc, UdpSocket);
