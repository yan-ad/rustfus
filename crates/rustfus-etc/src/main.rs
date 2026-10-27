#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod components;
mod system_font;
mod view;

fn main() -> iced::Result {
    use shadcn_iced::ApplicationExt;

    let font = system_font::configure_components();
    iced::application(app::App::boot, app::App::update, app::App::view)
        .title("RustFus")
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .shadcn_iced()
        .default_font(font)
        .window(iced::window::Settings {
            size: iced::Size::new(680.0, 820.0),
            min_size: Some(iced::Size::new(560.0, 600.0)),
            ..Default::default()
        })
        .run()
}
