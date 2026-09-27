//! Opt-in, low-overhead compositor timings. Enable with DEVEL_WM_PERF=1.

use std::time::{Duration, Instant};

pub struct PerfStats {
    since: Instant,
    events: u64,
    commits: u64,
    visible_commits: u64,
    refresh_us: u64,
    cleanup_us: u64,
    ipc_us: u64,
    redraw_requests: u64,
    render_us: u64,
    submit_us: u64,
    submitted: u64,
    damaged_pixels: u64,
    output_pixels: u64,
    frames_us: Vec<u64>,
}

impl Default for PerfStats {
    fn default() -> Self {
        Self {
            since: Instant::now(),
            events: 0,
            commits: 0,
            visible_commits: 0,
            refresh_us: 0,
            cleanup_us: 0,
            ipc_us: 0,
            redraw_requests: 0,
            render_us: 0,
            submit_us: 0,
            submitted: 0,
            damaged_pixels: 0,
            output_pixels: 0,
            frames_us: Vec::new(),
        }
    }
}

impl PerfStats {
    pub fn commit(&mut self, visible: bool) {
        self.commits += 1;
        self.visible_commits += u64::from(visible);
    }

    pub fn event(&mut self, refresh: Duration, cleanup: Duration, ipc: Duration, redraw: bool) {
        self.events += 1;
        self.refresh_us += micros(refresh);
        self.cleanup_us += micros(cleanup);
        self.ipc_us += micros(ipc);
        self.redraw_requests += u64::from(redraw);
        if self.since.elapsed() >= Duration::from_secs(1) {
            self.report();
        }
    }

    pub fn frame(
        &mut self,
        render: Duration,
        submit: Duration,
        damage: u64,
        output: u64,
        submitted: bool,
    ) {
        let render_us = micros(render);
        let submit_us = micros(submit);
        self.render_us += render_us;
        self.submit_us += submit_us;
        self.frames_us.push(render_us + submit_us);
        self.submitted += u64::from(submitted);
        self.damaged_pixels += damage;
        self.output_pixels += output;
    }

    fn report(&mut self) {
        if self.events == 0 && self.frames_us.is_empty() {
            return;
        }
        self.frames_us.sort_unstable();
        let p95 = if self.frames_us.is_empty() {
            0
        } else {
            self.frames_us[(self.frames_us.len() * 95 / 100).min(self.frames_us.len() - 1)]
        };
        eprintln!(
            "PERF interval_ms={} events={} commits={} visible_commits={} refresh_us={} cleanup_us={} ipc_us={} redraw_requests={} frames={} submitted={} render_us={} submit_us={} frame_p95_us={} damaged_pixels={} output_pixels={}",
            self.since.elapsed().as_millis(),
            self.events,
            self.commits,
            self.visible_commits,
            self.refresh_us,
            self.cleanup_us,
            self.ipc_us,
            self.redraw_requests,
            self.frames_us.len(),
            self.submitted,
            self.render_us,
            self.submit_us,
            p95,
            self.damaged_pixels,
            self.output_pixels,
        );
        self.since = Instant::now();
        self.events = 0;
        self.commits = 0;
        self.visible_commits = 0;
        self.refresh_us = 0;
        self.cleanup_us = 0;
        self.ipc_us = 0;
        self.redraw_requests = 0;
        self.render_us = 0;
        self.submit_us = 0;
        self.submitted = 0;
        self.damaged_pixels = 0;
        self.output_pixels = 0;
        self.frames_us.clear();
    }
}

impl Drop for PerfStats {
    fn drop(&mut self) {
        self.report();
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}
