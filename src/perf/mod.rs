//! Opt-in, low-overhead compositor timings. Enable with DEVEL_WM_PERF=1.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

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
    input_us: Vec<u64>,
    motion_flush_us: Vec<u64>,
    flip_us: Vec<u64>,
    // Time from queue_buffer to the DRM completion callback, by connector.
    frame_completion_us: BTreeMap<String, Vec<u64>>,
    wayland_us: Vec<u64>,
    resize_updates: u64,
    resize_configures: u64,
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
            input_us: Vec::new(),
            motion_flush_us: Vec::new(),
            flip_us: Vec::new(),
            frame_completion_us: BTreeMap::new(),
            wayland_us: Vec::new(),
            resize_updates: 0,
            resize_configures: 0,
        }
    }
}

impl PerfStats {
    pub fn input(&mut self, duration: Duration) {
        self.input_us.push(micros(duration));
    }

    pub fn motion_flush(&mut self, duration: Duration) {
        self.motion_flush_us.push(micros(duration));
    }

    pub fn frame_completed(&mut self, output: &str, duration: Duration) {
        self.frame_completion_us
            .entry(output.to_owned())
            .or_default()
            .push(micros(duration));
    }

    pub fn flip(&mut self, duration: Duration) {
        self.flip_us.push(micros(duration));
    }

    pub fn wayland(&mut self, duration: Duration) {
        self.wayland_us.push(micros(duration));
    }

    pub fn resize_update(&mut self) {
        self.resize_updates += 1;
    }

    pub fn resize_configure(&mut self) {
        self.resize_configures += 1;
    }

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
        if self.events == 0
            && self.frames_us.is_empty()
            && self.input_us.is_empty()
            && self.motion_flush_us.is_empty()
            && self.frame_completion_us.is_empty()
        {
            return;
        }
        let frame_p95 = p95(&mut self.frames_us);
        let input_p95 = p95(&mut self.input_us);
        let motion_flush_p95 = p95(&mut self.motion_flush_us);
        let flip_p95 = p95(&mut self.flip_us);
        let wayland_p95 = p95(&mut self.wayland_us);
        eprintln!(
            "PERF interval_ms={} events={} commits={} visible_commits={} refresh_us={} cleanup_us={} ipc_us={} redraw_requests={} frames={} submitted={} render_us={} submit_us={} frame_p95_us={} damaged_pixels={} output_pixels={} input_events={} input_p95_us={} input_max_us={} flips={} flip_p95_us={} flip_max_us={} wayland_dispatches={} wayland_p95_us={} wayland_max_us={} resize_updates={} resize_configures={}",
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
            frame_p95,
            self.damaged_pixels,
            self.output_pixels,
            self.input_us.len(),
            input_p95,
            self.input_us.last().copied().unwrap_or(0),
            self.flip_us.len(),
            flip_p95,
            self.flip_us.last().copied().unwrap_or(0),
            self.wayland_us.len(),
            wayland_p95,
            self.wayland_us.last().copied().unwrap_or(0),
            self.resize_updates,
            self.resize_configures,
        );
        eprintln!(
            "PERF_MOTION interval_ms={} flushes={} p95_us={} max_us={}",
            self.since.elapsed().as_millis(),
            self.motion_flush_us.len(),
            motion_flush_p95,
            self.motion_flush_us.last().copied().unwrap_or(0),
        );
        for (output, samples) in &mut self.frame_completion_us {
            let p95 = p95(samples);
            eprintln!(
                "PERF_OUTPUT output={} completed={} queue_to_callback_p95_us={} queue_to_callback_max_us={}",
                output,
                samples.len(),
                p95,
                samples.last().copied().unwrap_or(0),
            );
        }
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
        self.input_us.clear();
        self.motion_flush_us.clear();
        self.flip_us.clear();
        self.frame_completion_us.clear();
        self.wayland_us.clear();
        self.resize_updates = 0;
        self.resize_configures = 0;
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

fn p95(samples: &mut [u64]) -> u64 {
    samples.sort_unstable();
    samples
        .get((samples.len() * 95 / 100).min(samples.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_motion_and_frame_completions_are_scoped_to_an_interval_and_output() {
        let mut stats = PerfStats::default();
        stats.motion_flush(Duration::from_micros(12));
        stats.frame_completed("DP-4", Duration::from_micros(900));
        stats.frame_completed("HDMI-A-2", Duration::from_micros(400));
        stats.frame_completed("DP-4", Duration::from_micros(1100));
        assert_eq!(stats.motion_flush_us, [12]);
        assert_eq!(stats.frame_completion_us["DP-4"], [900, 1100]);
        assert_eq!(stats.frame_completion_us["HDMI-A-2"], [400]);
        stats.report();
        assert!(stats.motion_flush_us.is_empty());
        assert!(stats.frame_completion_us.is_empty());
    }
}
