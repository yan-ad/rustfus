use std::fmt;

use iced::widget::{column, row, rule, text};
use iced::{Element, Length, Theme};
use shadcn_iced::tokens;

#[derive(Debug, Clone)]
pub(crate) struct Choice<T> {
    pub value: T,
    pub label: String,
}

impl<T: PartialEq> PartialEq for Choice<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq> Eq for Choice<T> {}

impl<T> Choice<T> {
    pub fn new(value: T, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
        }
    }
}

impl<T> fmt::Display for Choice<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

pub(crate) fn section<'a, Message: 'a>(
    title: &'a str,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![
        row![text(title).size(22), rule::horizontal(1)]
            .spacing(12)
            .align_y(iced::Center),
        content.into(),
    ]
    .spacing(14)
    .width(Length::Fill)
    .into()
}

pub(crate) fn field<'a, Message: 'a>(
    label: &'a str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![text(label).size(14), control.into()]
        .spacing(7)
        .width(Length::Fill)
        .into()
}

pub(crate) fn note<'a, Message: 'a>(
    content: impl Into<String>,
    theme: &Theme,
) -> Element<'a, Message> {
    text(content.into())
        .size(12)
        .color(tokens(theme).palette.muted_foreground)
        .into()
}
