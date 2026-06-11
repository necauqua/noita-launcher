use indicatif::ProgressStyle;
use yansi::Paint;

#[derive(Clone)]
pub struct Printer(indicatif::MultiProgress);

impl Printer {
    pub fn new(quiet: bool) -> Self {
        let p = indicatif::MultiProgress::new();
        if quiet {
            p.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        Self(p)
    }

    pub fn hint(&self, msg: impl Into<String>) {
        self.0.println(msg.into().dim().to_string()).unwrap();
    }

    pub fn warn(&self, msg: impl Into<String>) {
        self.0
            .println(msg.into().dim().yellow().to_string())
            .unwrap();
    }

    pub fn error(&self, msg: impl Into<String>) {
        self.0.println(msg.into().bold().red().to_string()).unwrap();
    }

    pub fn bar(&mut self, len: u64) -> indicatif::ProgressBar {
        let bar = indicatif::ProgressBar::new(len).with_style(
            ProgressStyle::with_template(
                "{prefix:.dim} {spinner:.green} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-"),
        );
        self.0.add(bar.clone());
        bar
    }
}
