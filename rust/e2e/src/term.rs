use indicatif::ProgressStyle;
use std::time::Duration;
use tokio::{
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    time::interval,
};

pub async fn delay(msg: &str, sec: u64) {
    let steps = sec * 1000;
    let pb = indicatif::ProgressBar::new(steps);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{prefix:.bold.dim} {spinner:.green} [{bar:40.cyan/blue}] ({eta})")
            .progress_chars("-🐌."),
    );
    pb.set_prefix(&format!("{}", msg));

    let _ = tokio::spawn(async move {
        let mut intv = interval(Duration::from_millis(1));

        for _ in 0..steps {
            intv.tick().await;
            pb.inc(1);
        }
    })
    .await;
}

pub struct ProgressBar {
    receiver: UnboundedReceiver<()>,
    steps: u64,
}

impl ProgressBar {
    pub fn new(steps: u64, msg: &str) -> UnboundedSender<()> {
        let (tx, rx) = unbounded_channel::<()>();
        Self {
            receiver: rx,
            steps,
        }
        .run(msg);

        tx
    }

    fn run(self, msg: &str) {
        let Self {
            mut receiver,
            steps,
        } = self;

        let pb = indicatif::ProgressBar::new(steps);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{prefix:.bold.dim} [{elapsed_precise}] {spinner:.green} [{bar:40.cyan/blue}] {pos:2}/{len:2}")
                .progress_chars("-🐌."),
        );
        pb.set_prefix(&format!("Generate {}", msg));

        let _ = tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Some(_) => {
                        pb.inc(1);
                    }
                    None => {
                        warn!("dropped");
                        return;
                    }
                }
            }
        });
    }
}

pub fn spinner(prefix: &str, msg: &str) -> indicatif::ProgressBar {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(120);
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&[
                "▹▹▹▹▹",
                "▸▹▹▹▹",
                "▹▸▹▹▹",
                "▹▹▸▹▹",
                "▹▹▹▸▹",
                "▹▹▹▹▸",
                "▪▪▪▪▪",
            ])
            .template("{prefix:.bold.dim} {spinner:.blue} [{elapsed_precise}] {msg}"),
    );
    pb.set_message(msg);
    pb.set_prefix(prefix);
    return pb;
}
