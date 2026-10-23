//! Queue policy: at most `max_concurrent` running, the rest start in FIFO order.

use spate_proto::JobState;

/// Given `(index, state, seq)` for every job, the indexes of queued jobs to start now, oldest
/// queue position (`seq`) first, so that no more than `max_concurrent` run at once.
pub fn to_start(jobs: &[(usize, JobState, u64)], max_concurrent: usize) -> Vec<usize> {
    let running = jobs.iter().filter(|(_, s, _)| *s == JobState::Running).count();
    let free = max_concurrent.max(1).saturating_sub(running);
    let mut queued: Vec<&(usize, JobState, u64)> = jobs.iter().filter(|(_, s, _)| *s == JobState::Queued).collect();
    queued.sort_by_key(|(_, _, seq)| *seq);
    queued.into_iter().take(free).map(|(i, _, _)| *i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use JobState::*;

    #[test]
    fn fills_free_slots_fifo() {
        // Four queued, nothing running: the three oldest start.
        let jobs = [(0, Queued, 4), (1, Queued, 1), (2, Queued, 3), (3, Queued, 2)];
        assert_eq!(to_start(&jobs, 3), vec![1, 3, 2]);
    }

    #[test]
    fn respects_running_and_ignores_other_states() {
        let jobs = [(0, Running, 1), (1, Running, 2), (2, Queued, 9), (3, Queued, 5), (4, Paused, 0), (5, Done, 0)];
        assert_eq!(to_start(&jobs, 3), vec![3]);
        let full = [(0, Running, 1), (1, Running, 2), (2, Running, 3), (3, Queued, 4)];
        assert!(to_start(&full, 3).is_empty());
        // More running than allowed (setting lowered): nothing new starts.
        assert!(to_start(&full, 2).is_empty());
    }

    #[test]
    fn a_resumed_job_goes_to_the_back() {
        // Job 0 was resumed after 1 and 2 were queued: it got a newer seq.
        let jobs = [(0, Queued, 10), (1, Queued, 7), (2, Queued, 8), (3, Running, 1), (4, Running, 2)];
        assert_eq!(to_start(&jobs, 3), vec![1]);
        assert_eq!(to_start(&jobs, 5), vec![1, 2, 0]);
    }

    #[test]
    fn zero_means_one() {
        assert_eq!(to_start(&[(0, Queued, 1), (1, Queued, 2)], 0), vec![0]);
    }
}
