use std::sync::Arc;
use std::time::Instant;

use str0m::media::MediaTime;
use tokio::sync::{broadcast, mpsc};

/// Container of media data from the hardware codec to each client.
#[derive(Debug, Clone)]
pub struct MediaData {
    /// "Wall clock" of when the camera was captured. See str0m doc for media writer for
    /// more info on wallclock vs time.
    pub wallclock: Instant,
    /// Timestamp of this media data in "codec time", which is 90_000Hz for video.
    pub time: MediaTime,
    /// The actual data. We wrap this in Arc, to make MediaData cheaply clonable for each client.
    pub data: Arc<Vec<u8>>,
}

pub async fn cam_loop(tx_cam: broadcast::Sender<MediaData>, rx_keyframe: mpsc::Receiver<()>) {
    //
}
