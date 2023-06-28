use std::time::Instant;

use anyhow::{bail, Result};
use str0m::format::Codec;
use str0m::media::{Media, Mid, Pt};
use str0m::net::Receive;
use str0m::{Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc};

use crate::cam_loop::MediaData;

#[derive(Debug)]
pub struct Client {
    pub rtc: Rtc,
    pub socket: UdpSocket,
    pub rx_cam: broadcast::Receiver<MediaData>,
    pub tx_keyframe: mpsc::Sender<()>,
}

pub async fn run_loop(client: Client) {
    if let Err(error) = do_run(client).await {
        warn!("Client run_loop ended: {:?}", error);
    }
}

async fn do_run(mut client: Client) -> Result<()> {
    // To send the media to the client, we must know the identifier of the media in the
    // SDP. This is advertised in the first OFFER/ANSWER.
    //
    // Each media line (identified by mid) can negotiate multiple codec formats that _could_ be
    // used. In theory we could have a media line that alternates between VP8 and H264. However
    // our camera capture only produces H264 with a specific profile. This is the Pt (payload type)
    // that identifies which codec that is.
    let mut mid_and_pt: Option<(Mid, Pt)> = None;

    // Set to true when we want a keyframe from the h264 encoder.
    let mut request_keyframe = false;

    // Buffer to receive network data into. The MTU for UDP is pretty much always < 2000.
    let mut receive_buffer = vec![0_u8; 2000];

    loop {
        if request_keyframe {
            // Notify the h264 encoder that we want a new keyframe.
            client.tx_keyframe.send(()).await?;
            request_keyframe = false;
        }

        let timeout = match client.rtc.poll_output()? {
            Output::Timeout(v) => v,

            Output::Transmit(t) => {
                // str0m wants to send some data over the UDP socket.
                client.socket.send_to(&t.contents, t.destination).await?;

                continue; // poll more output
            }

            Output::Event(event) => {
                match event {
                    // The initial OFFER from the html web page requests a single video media (m-line).
                    // The media this creates inside str0m is advertised as soon as we start doing
                    // poll_output. We assume there is only one single media line added.
                    Event::MediaAdded(m) => {
                        // Unwrap here is ok because m.mid _must_ correspond to a Media.
                        let media = client.rtc.media(m.mid).unwrap();

                        // The Pt to use (see doc for field mid_and_pt above).
                        let pt = identify_pt_for_codec(media)?;

                        info!("mid: {}, pt: {}", m.mid, pt);
                        mid_and_pt = Some((m.mid, pt));
                    }

                    // The client wants a keyframe. Technically we get a mid in here, but since
                    // we only have a single media, we know what the keyframe is for.
                    Event::KeyframeRequest(_) => {
                        info!("Client requested keyframe");
                        request_keyframe = true;
                    }

                    // Client disconnected. End the loop.
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        info!("Client disconnected");
                        return Ok(());
                    }

                    _ => {} // ignore all other events.
                }

                continue; // poll more output
            }
        };

        // Timeout should be in the future or past, this gives us the duration or 0 if it's past.
        let wait_for = timeout.saturating_duration_since(Instant::now());

        if wait_for.is_zero() {
            // When str0m wants us to poll_output() straight away, we get a time in the past.
            // We also can't wait for 0 seconds.
            continue; // poll more output
        }

        // Either we timeout and need to do poll_output again, or we get some camera capture data
        // that needs to be sent to the client. This is where async shines - we can wait for multiple
        // things to happen.
        let cam_data = tokio::select! {
            _ = tokio::time::sleep(wait_for) => {
                // Finished timeout.
                continue; // poll more output.
            },

            // Read from the UdpSocket incoming data from the client.
            v = read_socket_input(&mut client.socket, &mut receive_buffer) => {
                if let Some(input) = v? {
                    // Parsed network input.
                    client.rtc.handle_input(input)?;
                }

                continue; // poll more output.
            },

            // Read from cam_loop encoded h264 data.
            v = client.rx_cam.recv() => {
                match v {
                    Ok(v) => v,
                    Err(RecvError::Closed) => {
                        bail!("Camera data sender is gone");
                    },
                    Err(RecvError::Lagged(_)) => {
                        // Camera data was produced faster than we managed to ship it to the
                        // Rtc client. This is unlikely to happen, but means we need to resync with
                        // a keyframe.
                        warn!("Cam data receiver lagged. Requesting keyframe");
                        request_keyframe = true;
                        continue;
                    },
                }
            },
        };

        let Some((mid, pt)) = mid_and_pt else {
            // If we don't have a mid/pt, we can't handle the cam data.
            continue;
        };

        // We need a media writer for the mid/pt combo.
        let mut media = client.rtc.media(mid).expect("Media line for mid");
        let writer = media.writer(pt);

        writer.write(cam_data.wallclock, cam_data.time, &cam_data.data)?;
    }
}

fn identify_pt_for_codec(media: Media<'_>) -> Result<Pt> {
    for p in media.payload_params() {
        let spec = p.spec();

        // Our hardware encoder produces h264.
        if spec.codec != Codec::H264 {
            continue;
        }

        // The hardware encoder produces this h264 profile level.
        if spec.format.profile_level_id != Some(0x12345) {
            continue;
        }

        // This is the codec we want.
        return Ok(p.pt());
    }

    bail!("Failed to find a usable codec");
}

async fn read_socket_input<'a>(
    socket: &UdpSocket,
    buf: &'a mut Vec<u8>,
) -> Result<Option<Input<'a>>> {
    // Prepare buffer for receiving.
    buf.resize(2000, 0);

    let (length, source) = socket.recv_from(buf).await?;
    buf.truncate(length);

    // Parse data to a DatagramRecv, which help preparse network data to
    // figure out the multiplexing of all protocols on one UDP port.
    let Ok(contents) = buf.as_slice().try_into() else {
        // An open socket might receive crap. Ignore that.
        return Ok(None);
    };

    return Ok(Some(Input::Receive(
        Instant::now(),
        Receive {
            source,
            destination: socket.local_addr().unwrap(),
            contents,
        },
    )));
}
