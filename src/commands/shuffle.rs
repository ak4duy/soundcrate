use rand::seq::SliceRandom;
use songbird::tracks::TrackQueue;

pub fn shuffle_upcoming(queue: &TrackQueue) {
    queue.modify_queue(|tracks| {
        if tracks.len() > 2 {
            tracks.make_contiguous()[1..].shuffle(&mut rand::rng());
        }
    });
}
