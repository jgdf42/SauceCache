//! Native edit menus that use the existing controls' selection and editing behavior.
use iced::advanced::{
    Clipboard, Layout, Shell, Widget, clipboard, layout, mouse, overlay, renderer,
    widget::{Operation, Tree, operation, tree},
};
use iced::widget::{text_editor, text_input};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, keyboard};

type InputState = text_input::State<<Renderer as iced::advanced::text::Renderer>::Paragraph>;

#[derive(Clone, Copy)]
enum Command {
    Cut,
    Copy,
    Paste,
    SelectAll,
}
enum Kind<'a> {
    Input(&'a str),
    Editor(&'a text_editor::Content),
    ReadOnly {
        copy: Box<dyn Fn() -> String + 'a>,
        has_text: bool,
    },
}
#[derive(Default)]
struct State {
    modifiers: keyboard::Modifiers,
    focused: bool,
    visible_focus: bool,
}
impl operation::Focusable for State {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
        self.visible_focus = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
        self.visible_focus = false;
    }
}

pub struct TextMenu<'a, M> {
    content: Element<'a, M>,
    kind: Kind<'a>,
    on_focus: Option<fn(Rectangle) -> M>,
}
impl<'a, M: Clone + 'a> TextMenu<'a, M> {
    pub fn copy_area(
        content: impl Into<Element<'a, M>>,
        has_text: bool,
        copy: impl Fn() -> String + 'a,
    ) -> Self {
        Self {
            content: content.into(),
            kind: Kind::ReadOnly {
                copy: Box::new(copy),
                has_text,
            },
            on_focus: None,
        }
    }
    pub fn input(content: impl Into<Element<'a, M>>, value: &'a str) -> Self {
        Self {
            content: content.into(),
            kind: Kind::Input(value),
            on_focus: None,
        }
    }
    pub fn editor(content: impl Into<Element<'a, M>>, value: &'a text_editor::Content) -> Self {
        Self {
            content: content.into(),
            kind: Kind::Editor(value),
            on_focus: None,
        }
    }
    pub fn on_focus(mut self, on_focus: fn(Rectangle) -> M) -> Self {
        self.on_focus = Some(on_focus);
        self
    }
    fn selection(&self, tree: &Tree) -> Option<String> {
        match self.kind {
            Kind::Input(value) => {
                let value = text_input::Value::new(value);
                native_input(tree)?
                    .cursor()
                    .selection(&value)
                    .map(|(a, b)| value.select(a, b).to_string())
            }
            Kind::Editor(content) => content.selection().filter(|text| !text.is_empty()),
            Kind::ReadOnly { ref copy, has_text } => has_text.then(copy),
        }
    }
    fn action(
        &mut self,
        command: Command,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
    ) {
        if matches!(self.kind, Kind::ReadOnly { .. }) && !matches!(command, Command::Copy) {
            return;
        }
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout,
            renderer,
            &mut Focus::default(),
        );
        // Explicit menu Copy/Cut also work on selected password text, without unmasking the field.
        let key = match (command, &self.kind) {
            (Command::Copy, _) => {
                if let Some(selected) = self.selection(&tree.children[0]) {
                    clipboard.write(clipboard::Kind::Standard, selected);
                }
                return;
            }
            (Command::Cut, Kind::Input(_)) => {
                let Some(selected) = self.selection(&tree.children[0]) else {
                    return;
                };
                clipboard.write(clipboard::Kind::Standard, selected);
                keyboard::Key::Named(keyboard::key::Named::Delete)
            }
            (Command::Cut, _) => {
                if self.selection(&tree.children[0]).is_none() {
                    return;
                }
                keyboard::Key::Character("x".into())
            }
            (Command::Paste, _) => keyboard::Key::Character("v".into()),
            (Command::SelectAll, _) => keyboard::Key::Character("a".into()),
        };
        let modifiers = if matches!(key, keyboard::Key::Named(_)) {
            keyboard::Modifiers::default()
        } else {
            keyboard::Modifiers::CTRL
        };
        let original = tree.state.downcast_ref::<State>().modifiers;
        for event in shortcut(key, modifiers, original) {
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                &event,
                layout,
                mouse::Cursor::Unavailable,
                renderer,
                clipboard,
                shell,
                &layout.bounds(),
            );
        }
        shell.request_redraw();
    }
}

fn native_input(tree: &Tree) -> Option<&InputState> {
    if tree.tag == tree::Tag::of::<InputState>() {
        return Some(tree.state.downcast_ref());
    }
    tree.children.iter().find_map(native_input)
}
#[derive(Default)]
struct Focus {
    bounds: Option<Rectangle>,
}
impl Operation for Focus {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
    fn focusable(
        &mut self,
        _: Option<&iced::advanced::widget::Id>,
        bounds: Rectangle,
        state: &mut dyn operation::Focusable,
    ) {
        self.bounds = Some(bounds);
        if !state.is_focused() {
            state.focus();
        }
    }
}
pub fn focus_only(target: Rectangle) -> impl Operation {
    struct FocusOnly(Rectangle);
    impl Operation for FocusOnly {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn focusable(
            &mut self,
            _: Option<&iced::advanced::widget::Id>,
            bounds: Rectangle,
            state: &mut dyn operation::Focusable,
        ) {
            if bounds == self.0 {
                if !state.is_focused() {
                    state.focus();
                }
            } else {
                state.unfocus();
            }
        }
    }
    FocusOnly(target)
}
fn shortcut(
    key: keyboard::Key,
    modifiers: keyboard::Modifiers,
    original: keyboard::Modifiers,
) -> [Event; 4] {
    let physical_key =
        keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
    [
        Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)),
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key,
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        }),
        Event::Keyboard(keyboard::Event::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key,
            location: keyboard::Location::Standard,
            modifiers,
        }),
        Event::Keyboard(keyboard::Event::ModifiersChanged(original)),
    ]
}

#[cfg(windows)]
fn popup(selected: bool, can_paste: bool, has_text: bool, read_only: bool) -> Option<Command> {
    use windows_sys::Win32::{Foundation::POINT, UI::WindowsAndMessaging::*};
    unsafe {
        let owner = GetForegroundWindow();
        let mut point = POINT { x: 0, y: 0 };
        if owner.is_null() || GetCursorPos(&mut point) == 0 {
            return None;
        }
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        for (id, label, enabled) in [
            (1, "Cut\tCtrl+X", selected),
            (2, "Copy\tCtrl+C", selected),
            (3, "Paste\tCtrl+V", can_paste),
            (4, "Select All\tCtrl+A", has_text),
        ] {
            if read_only && id != 2 {
                continue;
            }
            if id == 4 {
                AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            }
            let label: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
            AppendMenuW(
                menu,
                if enabled {
                    MF_STRING
                } else {
                    MF_STRING | MF_GRAYED
                },
                id,
                label.as_ptr(),
            );
        }
        let result = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            owner,
            std::ptr::null(),
        );
        DestroyMenu(menu);
        match result {
            1 => Some(Command::Cut),
            2 => Some(Command::Copy),
            3 => Some(Command::Paste),
            4 => Some(Command::SelectAll),
            _ => None,
        }
    }
}
#[cfg(not(windows))]
fn popup(_: bool, _: bool, _: bool, _: bool) -> Option<Command> {
    None
}

impl<'a, M: Clone + 'a> Widget<M, Theme, Renderer> for TextMenu<'a, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content]);
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if matches!(self.kind, Kind::ReadOnly { .. }) {
            operation.focusable(None, layout.bounds(), tree.state.downcast_mut::<State>());
        }
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) = event {
            tree.state.downcast_mut::<State>().modifiers = *modifiers;
        }
        let read_only = matches!(self.kind, Kind::ReadOnly { .. });
        if read_only
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            )
        {
            let state = tree.state.downcast_mut::<State>();
            state.focused = cursor.is_over(layout.bounds()) && cursor.is_over(*viewport);
            state.visible_focus = false;
        }
        if read_only
            && tree.state.downcast_ref::<State>().focused
            && let Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Character(key),
                modifiers,
                ..
            }) = event
            && key.eq_ignore_ascii_case("c")
            && modifiers.command()
        {
            self.action(Command::Copy, tree, layout, renderer, clipboard, shell);
            shell.capture_event();
            return;
        }
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
        ) && cursor.is_over(layout.bounds())
            && viewport.contains(cursor.position().unwrap())
        {
            let mut focus = Focus::default();
            if read_only {
                tree.state.downcast_mut::<State>().focused = true;
            }
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                &mut focus,
            );
            let selected = if let Kind::ReadOnly { has_text, .. } = self.kind {
                has_text
            } else {
                self.selection(&tree.children[0]).is_some()
            };
            let has_text = match self.kind {
                Kind::Input(value) => !value.is_empty(),
                Kind::Editor(value) => !value.is_empty(),
                Kind::ReadOnly { has_text, .. } => has_text,
            };
            let can_paste = !read_only
                && clipboard
                    .read(clipboard::Kind::Standard)
                    .is_some_and(|text| !text.is_empty());
            if let Some(command) = popup(selected, can_paste, has_text, read_only) {
                self.action(command, tree, layout, renderer, clipboard, shell);
            }
            if let (Some(on_focus), Some(bounds)) = (
                self.on_focus,
                focus.bounds.or(read_only.then_some(layout.bounds())),
            ) {
                shell.publish(on_focus(bounds));
            }
            shell.capture_event();
            shell.request_redraw();
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        let state = tree.state.downcast_ref::<State>();
        if matches!(self.kind, Kind::ReadOnly { .. }) && state.focused && state.visible_focus {
            use iced::advanced::renderer::Renderer as _;
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: iced::Border {
                        width: 2.0,
                        color: theme.extended_palette().primary.base.color,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                },
                iced::Color::TRANSPARENT,
            );
        }
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
impl<'a, M: Clone + 'a> From<TextMenu<'a, M>> for Element<'a, M> {
    fn from(menu: TextMenu<'a, M>) -> Self {
        Element::new(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activity_copy_copies_retained_history_without_allowing_edits() {
        let renderer = Renderer::new(iced::Font::DEFAULT, iced::Pixels(16.0));
        let mut menu: TextMenu<'_, String> =
            TextMenu::copy_area(iced::widget::text("latest\nolder"), true, || {
                "latest\nolder".into()
            });
        let (mut tree, node) = prepare(&mut menu, &renderer);
        let mut clipboard = ClipboardFixture::default();
        let mut messages = vec![];
        menu.action(
            Command::Copy,
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
        );
        assert_eq!(clipboard.0.as_deref(), Some("latest\nolder"));
        menu.action(
            Command::Paste,
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
        );
        assert!(messages.is_empty());
        tree.state.downcast_mut::<State>().focused = true;
        clipboard.0 = None;
        let event = shortcut(
            keyboard::Key::Character("c".into()),
            keyboard::Modifiers::CTRL,
            keyboard::Modifiers::default(),
        )[1]
        .clone();
        menu.update(
            &mut tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
            &Layout::new(&node).bounds(),
        );
        assert_eq!(clipboard.0.as_deref(), Some("latest\nolder"));
    }
    #[derive(Default)]
    struct ClipboardFixture(Option<String>);
    impl Clipboard for ClipboardFixture {
        fn read(&self, _: clipboard::Kind) -> Option<String> {
            self.0.clone()
        }
        fn write(&mut self, _: clipboard::Kind, text: String) {
            self.0 = Some(text);
        }
    }
    fn input_state(tree: &mut Tree) -> &mut InputState {
        if tree.tag == tree::Tag::of::<InputState>() {
            return tree.state.downcast_mut();
        }
        for child in &mut tree.children {
            if native_input(child).is_some() {
                return input_state(child);
            }
        }
        panic!("No native text input state");
    }
    fn prepare<'a, M: Clone + 'a>(
        menu: &mut TextMenu<'a, M>,
        renderer: &Renderer,
    ) -> (Tree, layout::Node) {
        let mut tree = Tree::new(menu as &dyn Widget<M, Theme, Renderer>);
        let node = menu.layout(
            &mut tree,
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(400.0, 160.0)),
        );
        (tree, node)
    }
    fn field(value: &str, secure: bool) -> TextMenu<'_, String> {
        let appearance = sk_iced::components::Appearance::fixed(
            iced::Font::DEFAULT,
            sk_iced::palette::palette(false, false),
        );
        TextMenu::input(
            sk_iced::components::TextInput::new(appearance, "", value)
                .secure(secure)
                .on_input(|s| s),
            value,
        )
    }
    #[test]
    fn copy_and_cut_preserve_unicode_selection_in_regular_and_password_fields() {
        let renderer = Renderer::new(iced::Font::DEFAULT, iced::Pixels(16.0));
        for secure in [false, true] {
            let mut menu = field("one βeta", secure);
            let (mut tree, node) = prepare(&mut menu, &renderer);
            input_state(&mut tree).focus();
            input_state(&mut tree).select_range(4, 8);
            let mut clipboard = ClipboardFixture::default();
            let mut messages = vec![];
            menu.action(
                Command::Copy,
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut clipboard,
                &mut Shell::new(&mut messages),
            );
            assert!(clipboard.0.as_deref() == Some("βeta"));
            assert_eq!(
                native_input(&tree)
                    .unwrap()
                    .cursor()
                    .selection(&text_input::Value::new("one βeta")),
                Some((4, 8))
            );
            menu.action(
                Command::Cut,
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut clipboard,
                &mut Shell::new(&mut messages),
            );
            assert_eq!(messages, vec!["one "]);
            input_state(&mut tree).select_range(1, 1);
            menu.action(
                Command::Cut,
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut clipboard,
                &mut Shell::new(&mut messages),
            );
            assert_eq!(
                messages.len(),
                1,
                "Cut without a selection must not delete a character"
            );
        }
    }
    #[test]
    fn switching_fields_unfocuses_other_inputs_without_losing_selection() {
        let renderer = Renderer::new(iced::Font::DEFAULT, iced::Pixels(16.0));
        let mut root: Element<'_, String> =
            iced::widget::row![field("first", false), field("second", false)].into();
        let mut tree = Tree::new(&root);
        let node = root.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(700.0, 50.0)),
        );
        input_state(&mut tree.children[0]).focus();
        input_state(&mut tree.children[1]).focus();
        input_state(&mut tree.children[1]).select_range(1, 4);
        let mut focus = Focus::default();
        root.as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut focus);
        root.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut focus_only(focus.bounds.unwrap()),
        );
        assert!(!native_input(&tree.children[0]).unwrap().is_focused());
        assert!(native_input(&tree.children[1]).unwrap().is_focused());
        assert_eq!(
            native_input(&tree.children[1])
                .unwrap()
                .cursor()
                .selection(&text_input::Value::new("second")),
            Some((1, 4))
        );
    }
    #[test]
    fn paste_replaces_selection_and_releases_shortcut_modifiers() {
        let renderer = Renderer::new(iced::Font::DEFAULT, iced::Pixels(16.0));
        let mut menu = field("abc", false);
        let (mut tree, node) = prepare(&mut menu, &renderer);
        input_state(&mut tree).focus();
        input_state(&mut tree).select_range(1, 2);
        let mut clipboard = ClipboardFixture(Some("\nZ".into()));
        let mut messages = vec![];
        menu.action(
            Command::Paste,
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
        );
        assert_eq!(messages, vec!["aZc"]);
        clipboard.0 = Some("Y".into());
        menu.action(
            Command::Paste,
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
        );
        assert_eq!(messages.last().unwrap(), "aZYc");
        menu.action(
            Command::SelectAll,
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
        );
        let mut event = shortcut(
            keyboard::Key::Character("c".into()),
            keyboard::Modifiers::default(),
            keyboard::Modifiers::default(),
        )[1]
        .clone();
        if let Event::Keyboard(keyboard::Event::KeyPressed { text, .. }) = &mut event {
            *text = Some("c".into());
        }
        menu.content.as_widget_mut().update(
            &mut tree.children[0],
            &event,
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
            &Layout::new(&node).bounds(),
        );
        assert_eq!(
            messages.last().unwrap(),
            "c",
            "Typing after a menu action must not leave Ctrl held"
        );
    }
    #[test]
    fn multiline_editor_menu_supports_select_all_copy_cut_and_paste() {
        let renderer = Renderer::new(iced::Font::DEFAULT, iced::Pixels(16.0));
        let mut content = text_editor::Content::with_text("first\nsecond");
        let mut clipboard = ClipboardFixture::default();
        for command in [
            Command::SelectAll,
            Command::Copy,
            Command::Cut,
            Command::Paste,
        ] {
            if matches!(command, Command::Paste) {
                clipboard.0 = Some("third\nfourth".into());
            }
            let messages = {
                let mut menu =
                    TextMenu::editor(text_editor(&content).on_action(|action| action), &content);
                let (mut tree, node) = prepare(&mut menu, &renderer);
                let mut messages = vec![];
                menu.action(
                    command,
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &mut clipboard,
                    &mut Shell::new(&mut messages),
                );
                messages
            };
            for action in messages {
                content.perform(action);
            }
            if matches!(command, Command::Copy) {
                assert_eq!(clipboard.0.as_deref(), Some("first\nsecond"));
            }
            if matches!(command, Command::Cut) {
                assert!(content.is_empty());
            }
        }
        assert_eq!(content.text(), "third\nfourth");
    }
}
