use indicatif::ProgressStyle;
use owo_colors::{OwoColorize as _, Stream, Style};

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
        let colored = msg
            .into()
            .if_supports_color(Stream::Stderr, |t| t.dimmed())
            .to_string();
        self.0.println(colored).unwrap();
    }

    pub fn warn(&self, msg: impl Into<String>) {
        let colored = msg
            .into()
            .if_supports_color(Stream::Stderr, |t| t.style(Style::new().dimmed().yellow()))
            .to_string();
        self.0.println(colored).unwrap();
    }

    pub fn error(&self, msg: impl Into<String>) {
        let colored = msg
            .into()
            .if_supports_color(Stream::Stderr, |t| t.style(Style::new().bold().red()))
            .to_string();
        self.0.println(colored).unwrap();
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
