//! Application-owned state and the first Desktop conversation composition.

use gpui::{
    Context, Entity, IntoElement, ParentElement as _, Render, Subscription, Window, prelude::*,
};
use gpui_ai::prelude::{
    Chat, ChatEvent, ChatMessage, ChatRole, ChatWelcome, MessageActions, ProgressState, PromptBar,
    PromptBarEvent, StreamedContent, Suggestion,
};
use gpui_component::{ActiveTheme as _, v_flex};
use std::sync::Arc;

/// Owns Xana Desktop's retained component entities and controlled transcript.
pub(crate) struct Workbench {
    chat: Entity<Chat>,
    messages: Arc<[ChatMessage]>,
    next_message_id: u64,
    _chat_subscription: Subscription,
}

impl Workbench {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prompt = cx.new(|cx| PromptBar::new("xana-composer", window, cx));
        prompt.update(cx, |prompt, cx| {
            prompt.set_progress(ProgressState::Pending, cx);
        });

        let chat = cx.new(|cx| Chat::new("xana-conversation", prompt, window, cx));
        chat.update(cx, |chat, cx| {
            chat.set_welcome(
                Some(
                    ChatWelcome::new("What can I help you with?")
                        .description("Xana Desktop is preparing its local runtime connection.")
                        .suggestions([Suggestion::new("capabilities", "What can Xana do?")]),
                ),
                cx,
            );
        });
        let subscription =
            cx.subscribe_in(&chat, window, |this, _, event: &ChatEvent, window, cx| {
                this.handle_chat_event(event, window, cx);
            });

        Self {
            chat,
            messages: Arc::from([]),
            next_message_id: 0,
            _chat_subscription: subscription,
        }
    }

    fn handle_chat_event(
        &mut self,
        event: &ChatEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ChatEvent::Prompt(PromptBarEvent::Submit { submission, .. }) => {
                self.push_message(
                    ChatRole::User,
                    StreamedContent::done(submission.text().to_string()),
                    window,
                    cx,
                );
                self.push_message(
                    ChatRole::System,
                    StreamedContent::failed(
                        String::new(),
                        "The local runtime connection is not ready yet.",
                    ),
                    window,
                    cx,
                );
            }
            ChatEvent::SuggestionSelected { suggestion_id }
                if suggestion_id.as_ref() == "capabilities" =>
            {
                self.chat.update(cx, |chat, cx| {
                    chat.prompt_bar().update(cx, |prompt, cx| {
                        prompt.set_draft("What can Xana do?", window, cx);
                    });
                });
            }
            _ => {}
        }
    }

    fn push_message(
        &mut self,
        role: ChatRole,
        content: StreamedContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.next_message_id = self.next_message_id.saturating_add(1);
        let id = format!("desktop-message-{}", self.next_message_id);
        let mut messages = self.messages.to_vec();
        messages.push(ChatMessage::new(id, role, content).actions(MessageActions::for_role(role)));
        self.messages = messages.into();
        let snapshot = self.messages.clone();
        self.chat.update(cx, |chat, cx| {
            chat.set_messages(snapshot, window, cx);
        });
        cx.notify();
    }
}

impl Render for Workbench {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.chat.clone())
    }
}
