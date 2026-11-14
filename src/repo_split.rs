//! A vertical splitter that reserves a readable Activity line below the fixed controls.
use iced::advanced::renderer::Renderer as _;
use iced::advanced::{
    Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
    widget::{Operation, Tree, tree},
};
use iced::{Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector};

const HANDLE: f32 = 10.0;
const GAP: f32 = 8.0;
const MIN_REPOS: f32 = 40.0;
// Activity title (22), gap (8), and a padded, single-line log viewport (30).
const MIN_ACTIVITY: f32 = 60.0;

#[derive(Default)]
struct Drag {
    offset: Option<f32>,
}

pub struct RepoSplit<'a, M> {
    children: [Element<'a, M>; 3],
    ratio: f32,
    on_resize: fn(f32) -> M,
}
impl<'a, M> RepoSplit<'a, M> {
    pub fn new(
        repos: impl Into<Element<'a, M>>,
        controls: impl Into<Element<'a, M>>,
        activity: impl Into<Element<'a, M>>,
        ratio: f32,
        on_resize: fn(f32) -> M,
    ) -> Self {
        Self {
            children: [repos.into(), controls.into(), activity.into()],
            ratio,
            on_resize,
        }
    }
}

fn repo_height(flexible: f32, ratio: f32) -> f32 {
    let maximum = (flexible - MIN_ACTIVITY).max(0.0);
    (flexible * ratio).clamp(MIN_REPOS.min(maximum), maximum)
}
fn handle(layout: Layout<'_>) -> Rectangle {
    let repos = layout.children().next().unwrap().bounds();
    Rectangle {
        x: repos.x,
        y: repos.y + repos.height,
        width: repos.width,
        height: HANDLE,
    }
}

impl<M> Widget<M, Theme, Renderer> for RepoSplit<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Drag>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Drag::default())
    }
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.max();
        let controls = self.children[1].as_widget_mut().layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let flexible = (size.height - controls.size().height - HANDLE - GAP).max(0.0);
        let top = repo_height(flexible, self.ratio);
        let bottom = flexible - top;
        let repos_size = Size::new(size.width, top);
        let activity_size = Size::new(size.width, bottom);
        let repos = self.children[0].as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(repos_size, repos_size),
        );
        let activity = self.children[2].as_widget_mut().layout(
            &mut tree.children[2],
            renderer,
            &layout::Limits::new(activity_size, activity_size),
        );
        let controls_height = controls.size().height;
        layout::Node::with_children(
            size,
            vec![
                repos,
                controls.move_to(Point::new(0.0, top + HANDLE)),
                activity.move_to(Point::new(0.0, top + HANDLE + controls_height + GAP)),
            ],
        )
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |op| {
            for ((child, state), bounds) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child.as_widget_mut().operate(state, bounds, renderer, op);
            }
        });
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
        let drag = tree.state.downcast_mut::<Drag>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if cursor.is_over(handle(layout)) =>
            {
                drag.offset = cursor.position().map(|p| p.y - handle(layout).y);
                shell.capture_event();
                shell.request_redraw();
                return;
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if drag.offset.is_some() => {
                let controls_height = layout.children().nth(1).unwrap().bounds().height;
                let flexible = (layout.bounds().height - controls_height - HANDLE - GAP).max(0.0);
                if flexible > 0.0 {
                    let requested =
                        (position.y - layout.bounds().y - drag.offset.unwrap()) / flexible;
                    shell.publish((self.on_resize)(
                        repo_height(flexible, requested) / flexible,
                    ));
                }
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if drag.offset.is_some() =>
            {
                drag.offset = None;
                shell.capture_event();
                shell.request_redraw();
                return;
            }
            Event::Window(iced::window::Event::Unfocused) => drag.offset = None,
            _ => {}
        }
        for ((child, state), bounds) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, bounds, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if tree.state.downcast_ref::<Drag>().offset.is_some() || cursor.is_over(handle(layout)) {
            return mouse::Interaction::ResizingVertically;
        }
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), bounds)| {
                child
                    .as_widget()
                    .mouse_interaction(state, bounds, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        for ((child, state), bounds) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, bounds, cursor, viewport);
        }
        let bar = handle(layout);
        let active = tree.state.downcast_ref::<Drag>().offset.is_some() || cursor.is_over(bar);
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bar.x,
                    y: bar.y + 4.0,
                    width: bar.width,
                    height: 2.0,
                },
                ..Default::default()
            },
            if active {
                theme.extended_palette().primary.base.color
            } else {
                theme.extended_palette().background.strong.color
            },
        );
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
impl<'a, M: 'a> From<RepoSplit<'a, M>> for Element<'a, M> {
    fn from(split: RepoSplit<'a, M>) -> Self {
        Element::new(split)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mouse_drag_clamps_releases_and_keeps_the_controls_in_place() {
        let renderer = Renderer::new(iced::Font::DEFAULT, iced::Pixels(16.0));
        let mut split = RepoSplit::new(
            iced::widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fill),
            iced::widget::Space::new().height(100),
            iced::widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fill),
            0.6,
            |ratio| ratio,
        );
        let mut tree = Tree::new(&split as &dyn Widget<f32, Theme, Renderer>);
        let limits = layout::Limits::new(Size::ZERO, Size::new(600.0, 400.0));
        let node = split.layout(&mut tree, &renderer, &limits);
        let bounds = Layout::new(&node);
        let initial = handle(bounds);
        let press = Point::new(100.0, initial.y + 5.0);
        let viewport = bounds.bounds();
        let mut clipboard = iced::advanced::clipboard::Null;
        let mut messages = vec![];
        split.update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            mouse::Cursor::Available(press),
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
            &viewport,
        );
        assert!(tree.state.downcast_ref::<Drag>().offset.is_some());
        // Drag beyond the window: controls keep their height, Activity keeps one line.
        let outside = Point::new(100.0, 1000.0);
        split.update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved { position: outside }),
            bounds,
            mouse::Cursor::Available(outside),
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
            &viewport,
        );
        split.ratio = messages[0];
        let expanded = split.layout(&mut tree, &renderer, &limits);
        let children = expanded.children();
        assert!((children[2].size().height - MIN_ACTIVITY).abs() < 0.001);
        assert_eq!(children[1].size().height, 100.0);
        assert!(children[0].size().height > node.children()[0].size().height);
        split.update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            Layout::new(&expanded),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
            &viewport,
        );
        assert!(tree.state.downcast_ref::<Drag>().offset.is_none());
        let sent = messages.len();
        split.update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved { position: press }),
            Layout::new(&expanded),
            mouse::Cursor::Available(press),
            &renderer,
            &mut clipboard,
            &mut Shell::new(&mut messages),
            &viewport,
        );
        assert_eq!(messages.len(), sent);
    }
    #[test]
    fn drag_bounds_reserve_one_activity_line() {
        assert_eq!(repo_height(200.0, 10.0), 140.0);
        assert_eq!(repo_height(200.0, -10.0), MIN_REPOS);
        assert_eq!(repo_height(70.0, 1.0), 10.0);
        assert_eq!(repo_height(0.0, 1.0), 0.0);
        assert_eq!(repo_height(200.0, 0.5), 100.0);
    }
}
