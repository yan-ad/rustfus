//! Capture the real application after discovery settles; no fixture devices are injected.

#[path = "../src/app.rs"]
mod app;
#[path = "../src/components.rs"]
mod components;
#[path = "../src/system_font.rs"]
mod system_font;
#[path = "../src/view.rs"]
mod view;

use iced::{Element, Subscription, Task};

#[derive(Debug, Clone)]
enum Message {
    App(app::Message),
    Capture,
    Captured(iced::window::Screenshot),
}

struct Preview(app::App);

impl Preview {
    fn new() -> (Self, Task<Message>) {
        let (app, boot) = app::App::boot();
        (
            Self(app),
            Task::batch([
                boot.map(Message::App),
                Task::perform(
                    async { tokio::time::sleep(std::time::Duration::from_secs(3)).await },
                    |_| Message::Capture,
                ),
            ]),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::App(message) => self.0.update(message).map(Message::App),
            Message::Capture => iced::window::oldest().then(|id| match id {
                Some(id) => iced::window::screenshot(id).map(Message::Captured),
                None => iced::exit(),
            }),
            Message::Captured(screenshot) => {
                let path = std::path::Path::new("artifacts/rustfus-iced-preview.png");
                std::fs::create_dir_all(path.parent().unwrap()).expect("create artifact directory");
                image::save_buffer(
                    path,
                    &screenshot.rgba,
                    screenshot.size.width,
                    screenshot.size.height,
                    image::ColorType::Rgba8,
                )
                .expect("save screenshot");
                println!("Saved {}", path.display());
                iced::exit()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        self.0.view().map(Message::App)
    }

    fn subscription(&self) -> Subscription<Message> {
        self.0.subscription().map(Message::App)
    }
}

fn main() -> iced::Result {
    use shadcn_iced::ApplicationExt;
    let font = system_font::configure_components();
    iced::application(Preview::new, Preview::update, Preview::view)
        .title("RustFus preview capture")
        .subscription(Preview::subscription)
        .theme(|preview: &Preview| preview.0.theme())
        .shadcn_iced()
        .default_font(font)
        .window_size((680.0, 820.0))
        .run()
}
