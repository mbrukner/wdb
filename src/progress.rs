use std::{
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

/// Progress is observational: a broken stderr must never abort flash programming.
pub struct Progress {
    label: String,
    stage: String,
    total: usize,
    done: usize,
    quiet: bool,
    terminal: bool,
    width: usize,
    last_draw: Instant,
    finished: bool,
}

impl Progress {
    pub fn new(label: impl Into<String>, total: usize, quiet: bool) -> Self {
        let mut progress = Self {
            label: label.into(),
            stage: String::new(),
            total: total.max(1),
            done: 0,
            quiet,
            terminal: io::stderr().is_terminal(),
            width: std::env::var("COLUMNS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(80usize)
                .clamp(20, 200),
            last_draw: Instant::now(),
            finished: false,
        };
        if progress.terminal {
            progress.draw(false);
        } else if !quiet {
            let _ = writeln!(io::stderr().lock(), "{}...", progress.label);
        }
        progress
    }

    pub fn update(&mut self, done: usize) {
        self.done = done.min(self.total);
        if self.terminal && self.last_draw.elapsed() >= Duration::from_millis(80) {
            self.draw(false);
        }
    }

    pub fn stage(&mut self, stage: &str, done: usize) {
        if self.stage == stage {
            self.update(done);
            return;
        }
        self.stage = stage.to_owned();
        self.done = done.min(self.total);
        if self.terminal {
            self.draw(false);
        } else if !self.quiet {
            let _ = writeln!(io::stderr().lock(), "{}: {}...", self.label, self.stage);
        }
    }

    pub fn finish(mut self) {
        self.done = self.total;
        self.finished = true;
        self.draw(true);
    }

    fn draw(&mut self, complete: bool) {
        if self.quiet {
            return;
        }
        // Reserve 100% for validated success, not merely receiving the last byte.
        let percent = if complete {
            100
        } else {
            (self.done * 100 / self.total).min(99)
        };
        let label = if self.stage.is_empty() {
            self.label.clone()
        } else {
            format!("{} {}", self.label, self.stage)
        };
        let line = bar(&label, percent, self.width);
        let mut stderr = io::stderr().lock();
        if self.terminal {
            let _ = write!(stderr, "\r\x1b[2K{line}");
            if complete {
                let _ = writeln!(stderr);
            }
        } else {
            let _ = writeln!(stderr, "{line}");
        }
        let _ = stderr.flush();
        self.last_draw = Instant::now();
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if !self.finished && !self.quiet {
            let mut stderr = io::stderr().lock();
            if self.terminal {
                let _ = write!(stderr, "\r\x1b[2K");
            }
            let _ = writeln!(stderr, "{}: failed", self.label);
        }
    }
}

fn bar(label: &str, percent: usize, width: usize) -> String {
    let label_width = 30.min(width.saturating_sub(16));
    let label: String = label.chars().take(label_width).collect();
    let cells = width.saturating_sub(label_width + 10).max(1);
    let filled = cells * percent / 100;
    format!(
        "{label:<label_width$} [{}{}] {percent:3}%",
        "#".repeat(filled),
        "-".repeat(cells - filled)
    )
}

#[cfg(test)]
mod tests {
    use super::bar;

    #[test]
    fn bars_fit_narrow_terminals_and_show_completion() {
        for width in [20, 40, 80, 200] {
            for percent in [0, 50, 100] {
                let line = bar("(1/5) Staging RAM", percent, width);
                assert!(line.len() < width);
                assert!(line.ends_with(&format!("{percent:3}%")));
            }
        }
    }
}
