#![allow(unused_variables)]

use crate::Message;
use cosmic::cctk::sctk::seat::keyboard::Keysym;
use cosmic::cctk::wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::__interfaces::zwp_keyboard_shortcuts_inhibitor_v1_interface;
use cosmic::cctk::wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1;
use cosmic::font::default;
use cosmic::iced::event::Status;
use cosmic::iced::keyboard::key::Named;
use cosmic::iced::mouse::Cursor;
use cosmic::iced::{Alignment, Event, Length, Limits, Rectangle, Size};
use cosmic::iced_core::layout::Node;
use cosmic::iced_core::renderer::Style;
use cosmic::iced_core::widget::Tree;
use cosmic::iced_core::{Clipboard, Layout, Shell};
use cosmic::widget::{self, button, icon, settings, text, Widget};
use cosmic::{iced, theme, Apply, Element, Renderer, Task, Theme};
use cosmic_config::{ConfigGet, ConfigSet};
use cosmic_settings_config::shortcuts::{self, Action, Binding, Modifiers, Shortcuts};
use cosmic_settings_page as page;
use itertools::Itertools;
use slab::Slab;
use slotmap::Key;
use std::borrow::Cow;
use std::io;
use std::str::FromStr;
use tracing::info;

#[derive(Clone, Debug)]
pub enum ShortcutMessage {
    AddKeybinding,
    ApplyReplace,
    CancelReplace,
    DeleteBinding(BindingId),
    DeleteShortcut(BindingId),
    EditBinding(BindingId, bool),
    InputBinding(BindingId, String),
    SubmitBinding(BindingId),
    PressBinding(BindingId),
    ResetBindings,
    ShowShortcut(BindingId, String),
    KeyPressed(BindingId, iced::keyboard::Key, iced::keyboard::Modifiers),
}

#[derive(Clone, Debug, Copy)]
pub struct BindingId(usize);

#[derive(Debug)]
pub struct ShortcutBinding {
    pub id: widget::Id,
    pub binding: Binding,
    pub input: String,
    pub is_default: bool,
    pub editing: bool,
}

#[must_use]
#[derive(Debug)]
pub struct ShortcutModel {
    pub action: Action,
    pub bindings: Slab<ShortcutBinding>,
    pub description: String,
    pub modified: u16,
    pub request_key_input: Option<BindingId>,
}

impl ShortcutModel {
    pub fn new(defaults: &Shortcuts, shortcuts: &Shortcuts, action: Action) -> Self {
        let (bindings, modified) =
            shortcuts
                .shortcuts(&action)
                .fold((Slab::new(), 0), |(mut slab, modified), binding| {
                    let is_default = defaults.0.get(binding) == Some(&action);

                    slab.insert(ShortcutBinding {
                        id: widget::Id::unique(),
                        binding: binding.clone(),
                        input: String::new(),
                        is_default,
                        editing: false,
                    });

                    (slab, if is_default { modified } else { modified + 1 })
                });

        let mut localized_description = super::localize_action(&action);
        if let Action::Spawn(_) = &action {
            localized_description = bindings
                .iter()
                .map(|(_, shortcut)| super::localize_custom_action(&action, &shortcut.binding))
                .take(1)
                .collect();
        }

        Self {
            description: localized_description,
            modified: defaults.0.iter().filter(|(_, a)| **a == action).fold(
                modified,
                |modified, (binding, _)| {
                    if bindings.iter().any(|(_, model)| model.binding == *binding) {
                        modified
                    } else {
                        modified + 1
                    }
                },
            ),
            action,
            bindings,
            request_key_input: None,
        }
    }
}

#[must_use]
pub struct Model {
    pub entity: page::Entity,
    pub defaults: Shortcuts,
    pub replace_dialog: Option<(BindingId, Binding, Action, String)>,
    pub shortcut_models: Slab<ShortcutModel>,
    pub shortcut_context: Option<BindingId>,
    pub config: cosmic_config::Config,
    pub custom: bool,
    pub actions: fn(&Shortcuts, &Shortcuts) -> Slab<ShortcutModel>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            entity: page::Entity::null(),
            defaults: Shortcuts::default(),
            replace_dialog: None,
            shortcut_models: Slab::new(),
            shortcut_context: None,
            config: shortcuts::context().unwrap(),
            custom: false,
            actions: |_, _| Slab::new(),
        }
    }
}

impl Model {
    pub fn actions(mut self, actions: fn(&Shortcuts, &Shortcuts) -> Slab<ShortcutModel>) -> Self {
        self.actions = actions;
        self
    }

    pub fn custom(mut self) -> Self {
        self.custom = true;
        self
    }

    /// Adds a new binding to the shortcuts config
    pub(super) fn config_add(&self, action: Action, binding: Binding) {
        let mut shortcuts = self.shortcuts_config();
        shortcuts.0.insert(binding, action);
        self.shortcuts_config_set(shortcuts);
    }

    /// Check if a binding is already set
    pub(super) fn config_contains(&self, binding: &Binding) -> Option<Action> {
        self.shortcuts_system_config()
            .0
            .get(binding)
            .cloned()
            .filter(|action| *action != Action::Disable)
    }

    /// Removes a binding from the shortcuts config
    pub(super) fn config_remove(&self, binding: &Binding) {
        let mut shortcuts = self.shortcuts_config();
        shortcuts.0.retain(|b, _| b != binding);
        self.shortcuts_config_set(shortcuts);
    }

    pub(super) fn context_drawer(&self) -> Option<Element<'_, ShortcutMessage>> {
        self.shortcut_context
            .as_ref()
            .map(|id| context_drawer(&self.shortcut_models, *id, self.custom))
    }

    pub(super) fn dialog(&self) -> Option<Element<'_, ShortcutMessage>> {
        if let Some(&(id, _, _, ref action)) = self.replace_dialog.as_ref() {
            if let Some(short_id) = self.shortcut_context {
                if let Some(model) = self.shortcut_models.get(short_id.0) {
                    if let Some(shortcut) = model.bindings.get(id.0) {
                        let primary_action = button::suggested(fl!("replace"))
                            .on_press(ShortcutMessage::ApplyReplace);

                        let secondary_action = button::standard(fl!("cancel"))
                            .on_press(ShortcutMessage::CancelReplace);

                        let dialog = widget::dialog()
                            .title(fl!("replace-shortcut-dialog"))
                            .icon(icon::from_name("dialog-warning").size(64))
                            .body(fl!(
                                "replace-shortcut-dialog",
                                "desc",
                                shortcut = shortcut.input.clone(),
                                name = shortcut
                                    .binding
                                    .description
                                    .as_ref()
                                    .unwrap_or(action)
                                    .to_owned()
                            ))
                            .primary_action(primary_action)
                            .secondary_action(secondary_action);

                        return Some(dialog.into());
                    }
                }
            }
        }

        None
    }

    pub(super) fn on_enter(&mut self) {
        let mut shortcuts = self.config.get::<Shortcuts>("defaults").unwrap_or_default();
        self.defaults = shortcuts.clone();

        if let Ok(custom) = self.config.get::<Shortcuts>("custom") {
            for (binding, action) in custom.0 {
                shortcuts.0.remove(&binding);
                shortcuts.0.insert(binding, action);
            }
        }

        self.shortcut_models = (self.actions)(&self.defaults, &shortcuts);
    }

    pub(super) fn on_clear(&mut self) {
        self.shortcut_models.clear();
        self.shortcut_models.shrink_to_fit();
    }

    /// Gets the custom configuration for keyboard shortcuts.
    pub(super) fn shortcuts_config(&self) -> Shortcuts {
        match self.config.get::<Shortcuts>("custom") {
            Ok(shortcuts) => shortcuts,
            Err(cosmic_config::Error::GetKey(_, why)) if why.kind() == io::ErrorKind::NotFound => {
                Shortcuts::default()
            }
            Err(why) => {
                tracing::error!(?why, "unable to get the current shortcuts config");
                Shortcuts::default()
            }
        }
    }

    /// Gets the system configuration for keyboard shortcuts.
    pub(super) fn shortcuts_system_config(&self) -> Shortcuts {
        let mut shortcuts = self.config.get::<Shortcuts>("defaults").unwrap_or_default();

        if let Ok(custom) = self.config.get::<Shortcuts>("custom") {
            shortcuts.0.extend(custom.0);
        }

        shortcuts
    }

    /// Writes a new configuration to the keyboard shortcuts config file.
    pub(super) fn shortcuts_config_set(&self, shortcuts: Shortcuts) {
        if let Err(why) = self.config.set("custom", shortcuts) {
            tracing::error!(?why, "failed to write shortcuts config");
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn update(&mut self, message: ShortcutMessage) -> Task<crate::app::Message> {
        match message {
            ShortcutMessage::AddKeybinding => {
                if let Some(short_id) = self.shortcut_context {
                    if let Some(model) = self.shortcut_models.get_mut(short_id.0) {
                        // If an empty entry exists, focus it instead of creating a new input.
                        for (_, shortcut) in &mut model.bindings {
                            if shortcut.binding.is_set()
                                || Binding::from_str(&shortcut.input).is_ok()
                            {
                                continue;
                            }

                            shortcut.input.clear();

                            return widget::text_input::focus(shortcut.id.clone());
                        }

                        // Create a new input and focus it.
                        let id = widget::Id::unique();
                        model.bindings.insert(ShortcutBinding {
                            id: id.clone(),
                            binding: Binding::default(),
                            input: String::new(),
                            is_default: false,
                            editing: false,
                        });

                        return widget::text_input::focus(id);
                    }
                }
            }

            ShortcutMessage::ApplyReplace => {
                if let Some((id, new_binding, ..)) = self.replace_dialog.take() {
                    if let Some(short_id) = self.shortcut_context {
                        // Remove conflicting bindings that are saved on disk.
                        self.config_remove(&new_binding);

                        // Clear any binding that matches this in the current model
                        for (_, model) in &mut self.shortcut_models {
                            if let Some(id) = model
                                .bindings
                                .iter()
                                .find(|(_, shortcut)| shortcut.binding == new_binding)
                                .map(|(id, _)| id)
                            {
                                model.bindings.remove(id);
                                break;
                            }
                        }

                        // Update the current model and save the binding to disk.
                        if let Some(model) = self.shortcut_models.get_mut(short_id.0) {
                            if let Some(shortcut) = model.bindings.get_mut(id.0) {
                                let prev_binding = shortcut.binding.clone();

                                shortcut.binding = new_binding.clone();
                                shortcut.input.clear();

                                let action = model.action.clone();
                                self.config_remove(&prev_binding);
                                self.config_add(action, new_binding);
                            }
                        }

                        self.on_enter();
                    }
                }
            }

            ShortcutMessage::CancelReplace => self.replace_dialog = None,

            ShortcutMessage::DeleteBinding(id) => {
                if let Some(short_id) = self.shortcut_context {
                    if let Some(model) = self.shortcut_models.get_mut(short_id.0) {
                        let shortcut = model.bindings.remove(id.0);
                        if shortcut.is_default {
                            self.config_add(Action::Disable, shortcut.binding.clone());
                        } else {
                            // if last keybind deleted, clear shortcut context
                            if model.bindings.is_empty() {
                                self.shortcut_context = None;
                            }
                            self.config_remove(&shortcut.binding);
                        }

                        self.on_enter();
                    }
                }
            }

            ShortcutMessage::DeleteShortcut(id) => {
                let model = self.shortcut_models.remove(id.0);
                for (_, shortcut) in model.bindings {
                    self.config_remove(&shortcut.binding);
                    self.on_enter();
                }
            }

            ShortcutMessage::EditBinding(id, enable) => {
                if let Some(shortcut) = self.shortcut_context
                    .and_then(|id| self.shortcut_models.get_mut(id.0))
                    .and_then(|model| model.bindings.get_mut(id.0)) {
                    shortcut.editing = enable;
                    if enable {
                        shortcut.input = shortcut.binding.to_string();
                        return widget::text_input::select_all(shortcut.id.clone());
                    } else if Binding::from_str(&shortcut.input).is_ok() {
                        return self.submit_binding(id);
                    }
                }
            }

            ShortcutMessage::InputBinding(id, text) => {
                if let Some(shortcut) = self.shortcut_context
                    .and_then(|id| self.shortcut_models.get_mut(id.0))
                    .and_then(|model| model.bindings.get_mut(id.0)) {
                    shortcut.input = text;
                }
            }

            ShortcutMessage::PressBinding(id) => {
                if let Some(model) = self.shortcut_context
                    .and_then(|id| self.shortcut_models.get_mut(id.0))
                    .take_if(|model| model.bindings.contains(id.0)) {
                    model.request_key_input = Some(id);
                }
            }

            // Removes all bindings from the active shortcut context, and reloads the shortcuts model.
            ShortcutMessage::ResetBindings => {
                if let Some(short_id) = self.shortcut_context {
                    if let Some(model) = self.shortcut_models.get(short_id.0) {
                        for (_, shortcut) in &model.bindings {
                            self.config_remove(&shortcut.binding);
                        }

                        if let Ok(defaults) = self.config.get::<Shortcuts>("defaults") {
                            for (binding, action) in defaults.0 {
                                if action == model.action {
                                    self.config_remove(&binding);
                                }
                            }
                        }
                    }

                    self.on_enter();
                }
            }

            ShortcutMessage::SubmitBinding(id) => return self.submit_binding(id),

            ShortcutMessage::ShowShortcut(id, description) => {
                self.shortcut_context = Some(id);
                self.replace_dialog = None;

                let mut tasks = vec![cosmic::task::message(
                    crate::app::Message::OpenContextDrawer(self.entity, description.into()),
                )];

                if let Some(model) = self.shortcut_models.get(0) {
                    if let Some(shortcut) = model.bindings.get(0) {
                        tasks.push(widget::text_input::focus(shortcut.id.clone()));
                        tasks.push(widget::text_input::select_all(shortcut.id.clone()));
                    }
                }

                return Task::batch(tasks);
            }

            ShortcutMessage::KeyPressed(binding_id, pressed_key, modifiers) => {
                let mut apply_binding = None;

                if let Some(model) = self.shortcut_context
                    .and_then(|id| self.shortcut_models.get_mut(id.0)) {
                    if let Some(shortcut) = model.bindings.get_mut(binding_id.0) {
                        if let KeysymValue(Some(keysym)) = pressed_key.into() {
                            let new_binding = Binding::new(Modifiers {
                                ctrl: modifiers.control(),
                                alt: modifiers.alt(),
                                shift: modifiers.shift(),
                                logo: modifiers.logo(),
                            }, Some(keysym));

                            shortcut.input = new_binding.to_string();
                            model.request_key_input = None;

                            let str = self.shortcuts_system_config().0.iter().map(|(b, _) | b.to_string()).join("\n");
                            info!("shortcuts system config:\n{}", str);

                            if let Some(action) = self.config_contains(&new_binding) {
                                let action_str = if let Action::Spawn(_) = &action {
                                    super::localize_custom_action(&action, &new_binding)
                                } else {
                                    super::localize_action(&action)
                                };

                                self.replace_dialog = Some((binding_id, new_binding, action, action_str));

                                return Task::none();
                            }


                            apply_binding = Some(new_binding);
                        }
                    }
                }

                if let Some(new_binding) = apply_binding {
                    if let Some(model) = self.shortcut_context
                        .and_then(|id| self.shortcut_models.get_mut(id.0)) {
                            if let Some(binding) = model.bindings.get_mut(binding_id.0) {
                                let prev_binding = binding.binding.clone();

                                binding.input = new_binding.to_string();
                                binding.binding = new_binding.clone();
                                binding.editing = false;

                                let action = model.action.clone();
                                self.config_remove(&prev_binding);
                                self.config_add(action, new_binding);
                                self.on_enter();
                            }
                        }
                }
            }
        }

        Task::none()
    }

    fn submit_binding(&mut self, id: BindingId) -> Task<Message> {
        if let Some(short_id) = self.shortcut_context {
            let mut apply_binding = None;

            // Check for conflicts with the new binding.
            if let Some(model) = self.shortcut_models.get_mut(short_id.0) {
                if let Some(shortcut) = model.bindings.get_mut(id.0) {
                    match Binding::from_str(&shortcut.input) {
                        Ok(new_binding) => {
                            if !new_binding.is_set() {
                                shortcut.input.clear();
                                return Task::none();
                            }

                            if let Some(action) = self.config_contains(&new_binding) {
                                let action_str = if let Action::Spawn(_) = &action {
                                    super::localize_custom_action(&action, &new_binding)
                                } else {
                                    super::localize_action(&action)
                                };

                                self.replace_dialog = Some((id, new_binding, action, action_str));

                                return Task::none();
                            }

                            apply_binding = Some(new_binding);
                        }

                        Err(why) => {
                            tracing::error!(why, "keybinding input invalid");
                        }
                    }
                }
            }

            // Apply if no conflict was found.
            if let Some(new_binding) = apply_binding {
                if let Some(model) = self.shortcut_models.get_mut(short_id.0) {
                    if let Some(shortcut) = model.bindings.get_mut(id.0) {
                        let prev_binding = shortcut.binding.clone();

                        shortcut.binding = new_binding.clone();
                        shortcut.input.clear();
                        shortcut.editing = false;

                        let action = model.action.clone();
                        self.config_remove(&prev_binding);
                        self.config_add(action, new_binding);
                        self.on_enter();
                    }
                }
            }
        }

        Task::none()
    }

    pub(super) fn view(&self) -> Element<ShortcutMessage> {
        self.shortcut_models
            .iter()
            .map(|(id, shortcut)| shortcut_item(self.custom, BindingId(id), shortcut))
            .fold(widget::list_column(), widget::ListColumn::add)
            .into()
    }
}

fn context_drawer(
    shortcuts: &Slab<ShortcutModel>,
    id: BindingId,
    show_action: bool,
) -> Element<ShortcutMessage> {
    let cosmic::cosmic_theme::Spacing {
        space_xxs,
        space_xs,
        space_l,
        ..
    } = theme::active().cosmic().spacing;

    let model = &shortcuts[id.0];

    let action = show_action.then(|| {
        let description = if let Action::Spawn(task) = &model.action {
            Cow::Borrowed(task.as_str())
        } else {
            Cow::Owned(super::localize_action(&model.action))
        };

        text::body(description)
    });

    let bindings = model.bindings.iter().enumerate().fold(
        widget::list_column().spacing(space_xxs),
        |section, (_, (bind_id, shortcut))| {
            let text: Cow<'_, str> = if shortcut.binding.is_set() {
                Cow::Owned(shortcut.binding.to_string())
            } else {
                Cow::Borrowed(&shortcut.input)
            };

            let input = widget::editable_input("", text, shortcut.editing, move |enable| {
                ShortcutMessage::EditBinding(BindingId(bind_id), enable)
            })
                .select_on_focus(true)
                .on_input(move |text| ShortcutMessage::InputBinding(BindingId(bind_id), text))
                .on_submit(ShortcutMessage::SubmitBinding(BindingId(bind_id)))
                .padding([0, space_xs])
                .id(shortcut.id.clone())
                .into();

            let delete_button = widget::button::icon(icon::from_name("edit-delete-symbolic"))
                .on_press(ShortcutMessage::DeleteBinding(BindingId(bind_id)))
                .into();

            let type_key_combination_button = widget::button::icon(icon::from_name("input-keyboard-symbolic"))
                .on_press(ShortcutMessage::PressBinding(BindingId(bind_id)))
                .into();

            let flex_control =
                settings::item_row(vec![input, delete_button, type_key_combination_button]).align_y(Alignment::Center);

            section.add(flex_control)
        },
    );

    // TODO: Detect when it is necessary
    let reset_keybinding_button = if show_action {
        None
    } else {
        let button = widget::button::standard(fl!("reset-to-default"))
            .on_press(ShortcutMessage::ResetBindings);
        Some(button)
    };

    let add_keybinding_button =
        widget::button::standard(fl!("add-keybinding")).on_press(ShortcutMessage::AddKeybinding);

    let button_container = widget::row::with_capacity(2)
        .push_maybe(reset_keybinding_button)
        .push(add_keybinding_button)
        .spacing(space_xs)
        .apply(widget::container)
        .width(Length::Fill)
        .align_x(Alignment::End);

    let mut capacity = 2;
    if show_action {
        capacity += 1;
    }

    let key_input = if let Some(binding_id) = &model.request_key_input {
        capacity += 1;

        Some(widget::row::with_capacity(2)
            .push(widget::container(text::body(fl!("type-key-combination"))))
            .push(InputKeyEventHandler {
                binding_id: *binding_id,
                on_key_pressed: Box::new(ShortcutMessage::KeyPressed),
            }))
    } else {
        None
    };

    widget::column::with_capacity(capacity)
        .spacing(space_l)
        .push_maybe(action)
        .push_maybe(key_input)
        .push(bindings)
        .push(button_container)
        .into()
}

struct InputKeyEventHandler<'a, Message>
{
    on_key_pressed: Box<dyn Fn(BindingId, iced::keyboard::Key, iced::keyboard::Modifiers) -> Message + 'a>,
    binding_id: BindingId,
}

impl<'a, Message> Widget<Message, Theme, Renderer> for InputKeyEventHandler<'a, Message> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(0.0), Length::Fixed(0.0))
    }

    fn layout(&self, _tree: &mut Tree, _renderer: &Renderer, _limits: &Limits) -> Node {
        Node::new(Size::ZERO)
    }

    fn draw(&self, _tree: &Tree, _renderer: &mut Renderer, _theme: &Theme, _style: &Style, _layout: Layout<'_>, cursor: Cursor, _viewport: &Rectangle) {}

    fn on_event(&mut self, _state: &mut Tree, event: Event, _layout: Layout<'_>, _cursor: Cursor, _renderer: &Renderer, _clipboard: &mut dyn Clipboard, shell: &mut Shell<'_, Message>, _viewport: &Rectangle) -> Status {
        match event {
            Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                shell.publish((self.on_key_pressed)(self.binding_id, key, modifiers));
                
                Status::Captured
            }
            _ => Status::Ignored
        }
    }
}

impl<'a, Message: 'a> From<InputKeyEventHandler<'a, Message>> for Element<'a, Message> {
    fn from(input_key_event_handler: InputKeyEventHandler<'a, Message>) -> Self {
        Element::new(input_key_event_handler)
    }
}

struct KeysymValue(Option<Keysym>);

impl From<KeysymValue> for Option<Keysym> {
    fn from(value: KeysymValue) -> Self {
        value.0
    }
}

impl From<Option<Keysym>> for KeysymValue {
    fn from(value: Option<Keysym>) -> Self {
        KeysymValue(value)
    }
}

impl From<iced::keyboard::Key> for KeysymValue {
    fn from(value: iced::keyboard::Key) -> Self {
        match value {
            iced::keyboard::Key::Named(named) => match named {
                Named::Alt => None,
                Named::AltGraph => Some(Keysym::SUN_AltGraph),
                Named::CapsLock => Some(Keysym::Caps_Lock),
                Named::Control => None,
                Named::Fn => Some(Keysym::XF86_Fn),
                Named::FnLock => None,
                Named::NumLock => Some(Keysym::Num_Lock),
                Named::ScrollLock => Some(Keysym::Scroll_Lock),
                Named::Shift => None,
                Named::Symbol => None,
                Named::SymbolLock => None,
                Named::Meta => None,
                Named::Hyper => Some(Keysym::Hyper_L),
                Named::Super => None,
                Named::Enter => Some(Keysym::Return),
                Named::Tab => Some(Keysym::Tab),
                Named::Space => Some(Keysym::space),
                Named::ArrowDown => Some(Keysym::Down),
                Named::ArrowLeft => Some(Keysym::Left),
                Named::ArrowRight => Some(Keysym::Right),
                Named::ArrowUp => Some(Keysym::Up),
                Named::End => Some(Keysym::End),
                Named::Home => Some(Keysym::Home),
                Named::PageDown => Some(Keysym::Page_Down),
                Named::PageUp => Some(Keysym::Page_Up),
                Named::Backspace => Some(Keysym::BackSpace),
                Named::Clear => Some(Keysym::Clear),
                Named::Copy => Some(Keysym::XF86_Copy),
                Named::CrSel => None,
                Named::Cut => Some(Keysym::XF86_Cut),
                Named::Delete => Some(Keysym::Delete),
                Named::EraseEof => None,
                Named::ExSel => None,
                Named::Insert => Some(Keysym::Insert),
                Named::Paste => Some(Keysym::XF86_Paste),
                Named::Redo => Some(Keysym::Redo),
                Named::Undo => Some(Keysym::Undo),
                Named::Accept => None,
                Named::Again => None,
                Named::Attn => None,
                Named::Cancel => Some(Keysym::Cancel),
                Named::ContextMenu => Some(Keysym::Menu),
                Named::Escape => Some(Keysym::Escape),
                Named::Execute => Some(Keysym::Execute),
                Named::Find => Some(Keysym::Find),
                Named::Help => Some(Keysym::Help),
                Named::Pause => Some(Keysym::Pause),
                Named::Play => None,
                Named::Props => Some(Keysym::SUN_Props),
                Named::Select => Some(Keysym::Select),
                Named::ZoomIn => Some(Keysym::XF86_ZoomIn),
                Named::ZoomOut => Some(Keysym::XF86_ZoomOut),
                Named::BrightnessDown => Some(Keysym::XF86_MonBrightnessDown),
                Named::BrightnessUp => Some(Keysym::XF86_MonBrightnessUp),
                Named::Eject => Some(Keysym::XF86_Eject),
                Named::LogOff => Some(Keysym::XF86_LogOff),
                Named::Power => Some(Keysym::SUN_PowerSwitch),
                Named::PowerOff => Some(Keysym::XF86_PowerOff),
                Named::PrintScreen => Some(Keysym::SUN_Print_Screen),
                Named::Hibernate => Some(Keysym::XF86_Hibernate),
                Named::Standby => Some(Keysym::XF86_Standby),
                Named::WakeUp => Some(Keysym::XF86_WakeUp),
                Named::AllCandidates => Some(Keysym::MultipleCandidate),
                Named::Alphanumeric => None,
                Named::CodeInput => None,
                Named::Compose => Some(Keysym::Multi_key),
                Named::Convert => None,
                Named::FinalMode => None,
                Named::GroupFirst => Some(Keysym::ISO_First_Group),
                Named::GroupLast => Some(Keysym::ISO_Last_Group),
                Named::GroupNext => Some(Keysym::ISO_Next_Group),
                Named::GroupPrevious => Some(Keysym::ISO_Prev_Group),
                Named::ModeChange => Some(Keysym::Mode_switch),
                Named::NextCandidate => None,
                Named::NonConvert => None,
                Named::PreviousCandidate => Some(Keysym::PreviousCandidate),
                Named::Process => None,
                Named::SingleCandidate => Some(Keysym::SingleCandidate),
                Named::HangulMode => Some(Keysym::Hangul),
                Named::HanjaMode => Some(Keysym::Hangul_Hanja),
                Named::JunjaMode => Some(Keysym::Hangul_Jeonja),
                Named::Eisu => Some(Keysym::Eisu_toggle),
                Named::Hankaku => Some(Keysym::Hankaku),
                Named::Hiragana => Some(Keysym::Hiragana),
                Named::HiraganaKatakana => Some(Keysym::Hiragana_Katakana),
                Named::KanaMode => Some(Keysym::Kana_Lock),
                Named::KanjiMode => Some(Keysym::Kanji),
                Named::Katakana => Some(Keysym::Katakana),
                Named::Romaji => Some(Keysym::Romaji),
                Named::Zenkaku => Some(Keysym::Zenkaku),
                Named::ZenkakuHankaku => Some(Keysym::Zenkaku_Hankaku),
                Named::Soft1 => None,
                Named::Soft2 => None,
                Named::Soft3 => None,
                Named::Soft4 => None,
                Named::ChannelDown => Some(Keysym::XF86_ChannelDown),
                Named::ChannelUp => Some(Keysym::XF86_ChannelUp),
                Named::Close => None,
                Named::MailForward => Some(Keysym::XF86_MailForward),
                Named::MailReply => Some(Keysym::XF86_Reply),
                Named::MailSend => Some(Keysym::XF86_Send),
                Named::MediaClose => None,
                Named::MediaFastForward => Some(Keysym::XF86_AudioForward),
                Named::MediaPause => Some(Keysym::XF86_AudioPause),
                Named::MediaPlay => None,
                Named::MediaPlayPause => None,
                Named::MediaRecord => Some(Keysym::XF86_AudioRecord),
                Named::MediaRewind => Some(Keysym::XF86_AudioRewind),
                Named::MediaStop => Some(Keysym::XF86_AudioStop),
                Named::MediaTrackNext => Some(Keysym::XF86_AudioNext),
                Named::MediaTrackPrevious => Some(Keysym::XF86_AudioPrev),
                Named::New => Some(Keysym::XF86_New),
                Named::Open => Some(Keysym::XF86_Open),
                Named::Print => Some(Keysym::Print),
                Named::Save => Some(Keysym::XF86_Save),
                Named::SpellCheck => Some(Keysym::XF86_Spell),
                Named::Key11 => Some(Keysym::XF86_Numeric11),
                Named::Key12 => Some(Keysym::XF86_Numeric12),
                Named::AudioBalanceLeft => None,
                Named::AudioBalanceRight => None,
                Named::AudioBassBoostDown => None,
                Named::AudioBassBoostToggle => None,
                Named::AudioBassBoostUp => None,
                Named::AudioFaderFront => None,
                Named::AudioFaderRear => None,
                Named::AudioSurroundModeNext => None,
                Named::AudioTrebleDown => None,
                Named::AudioTrebleUp => None,
                Named::AudioVolumeDown => Some(Keysym::XF86_AudioLowerVolume),
                Named::AudioVolumeUp => Some(Keysym::XF86_AudioRaiseVolume),
                Named::AudioVolumeMute => Some(Keysym::XF86_AudioMute),
                Named::MicrophoneToggle => None,
                Named::MicrophoneVolumeDown => None,
                Named::MicrophoneVolumeUp => None,
                Named::MicrophoneVolumeMute => Some(Keysym::XF86_AudioMicMute),
                Named::SpeechCorrectionList => None,
                Named::SpeechInputToggle => None,
                Named::LaunchApplication1 => Some(Keysym::XF86_MyComputer),
                Named::LaunchApplication2 => Some(Keysym::XF86_Calculator),
                Named::LaunchCalendar => Some(Keysym::XF86_Calendar),
                Named::LaunchContacts => None,
                Named::LaunchMail => Some(Keysym::XF86_Mail),
                Named::LaunchMediaPlayer => None,
                Named::LaunchMusicPlayer => Some(Keysym::XF86_AudioMedia),
                Named::LaunchPhone => Some(Keysym::XF86_Phone),
                Named::LaunchScreenSaver => Some(Keysym::XF86_ScreenSaver),
                Named::LaunchSpreadsheet => None,
                Named::LaunchWebBrowser => Some(Keysym::XF86_WWW),
                Named::LaunchWebCam => Some(Keysym::XF86_WebCam),
                Named::LaunchWordProcessor => Some(Keysym::XF86_Word),
                Named::BrowserBack => Some(Keysym::XF86_Back),
                Named::BrowserFavorites => Some(Keysym::XF86_Favorites),
                Named::BrowserForward => Some(Keysym::XF86_Forward),
                Named::BrowserHome => Some(Keysym::XF86_HomePage),
                Named::BrowserRefresh => Some(Keysym::XF86_Refresh),
                Named::BrowserSearch => Some(Keysym::XF86_Search),
                Named::BrowserStop => Some(Keysym::XF86_Stop),
                Named::AppSwitch => None,
                Named::Call => None,
                Named::Camera => None,
                Named::CameraFocus => None,
                Named::EndCall => None,
                Named::GoBack => None,
                Named::GoHome => None,
                Named::HeadsetHook => None,
                Named::LastNumberRedial => None,
                Named::Notification => None,
                Named::MannerMode => None,
                Named::VoiceDial => None,
                Named::TV => None,
                Named::TV3DMode => None,
                Named::TVAntennaCable => None,
                Named::TVAudioDescription => None,
                Named::TVAudioDescriptionMixDown => None,
                Named::TVAudioDescriptionMixUp => None,
                Named::TVContentsMenu => None,
                Named::TVDataService => None,
                Named::TVInput => None,
                Named::TVInputComponent1 => None,
                Named::TVInputComponent2 => None,
                Named::TVInputComposite1 => None,
                Named::TVInputComposite2 => None,
                Named::TVInputHDMI1 => None,
                Named::TVInputHDMI2 => None,
                Named::TVInputHDMI3 => None,
                Named::TVInputHDMI4 => None,
                Named::TVInputVGA1 => None,
                Named::TVMediaContext => None,
                Named::TVNetwork => None,
                Named::TVNumberEntry => None,
                Named::TVPower => None,
                Named::TVRadioService => None,
                Named::TVSatellite => None,
                Named::TVSatelliteBS => None,
                Named::TVSatelliteCS => None,
                Named::TVSatelliteToggle => None,
                Named::TVTerrestrialAnalog => None,
                Named::TVTerrestrialDigital => None,
                Named::TVTimer => None,
                Named::AVRInput => None,
                Named::AVRPower => None,
                Named::ColorF0Red => None,
                Named::ColorF1Green => None,
                Named::ColorF2Yellow => None,
                Named::ColorF3Blue => None,
                Named::ColorF4Grey => None,
                Named::ColorF5Brown => None,
                Named::ClosedCaptionToggle => None,
                Named::Dimmer => None,
                Named::DisplaySwap => None,
                Named::DVR => None,
                Named::Exit => None,
                Named::FavoriteClear0 => None,
                Named::FavoriteClear1 => None,
                Named::FavoriteClear2 => None,
                Named::FavoriteClear3 => None,
                Named::FavoriteRecall0 => None,
                Named::FavoriteRecall1 => None,
                Named::FavoriteRecall2 => None,
                Named::FavoriteRecall3 => None,
                Named::FavoriteStore0 => None,
                Named::FavoriteStore1 => None,
                Named::FavoriteStore2 => None,
                Named::FavoriteStore3 => None,
                Named::Guide => None,
                Named::GuideNextDay => None,
                Named::GuidePreviousDay => None,
                Named::Info => None,
                Named::InstantReplay => None,
                Named::Link => None,
                Named::ListProgram => None,
                Named::LiveContent => None,
                Named::Lock => None,
                Named::MediaApps => None,
                Named::MediaAudioTrack => None,
                Named::MediaLast => None,
                Named::MediaSkipBackward => None,
                Named::MediaSkipForward => None,
                Named::MediaStepBackward => None,
                Named::MediaStepForward => None,
                Named::MediaTopMenu => Some(Keysym::XF86_MediaTopMenu),
                Named::NavigateIn => None,
                Named::NavigateNext => None,
                Named::NavigateOut => None,
                Named::NavigatePrevious => None,
                Named::NextFavoriteChannel => None,
                Named::NextUserProfile => None,
                Named::OnDemand => None,
                Named::Pairing => None,
                Named::PinPDown => None,
                Named::PinPMove => None,
                Named::PinPToggle => None,
                Named::PinPUp => None,
                Named::PlaySpeedDown => None,
                Named::PlaySpeedReset => None,
                Named::PlaySpeedUp => None,
                Named::RandomToggle => Some(Keysym::XF86_AudioRandomPlay),
                Named::RcLowBattery => None,
                Named::RecordSpeedNext => None,
                Named::RfBypass => None,
                Named::ScanChannelsToggle => None,
                Named::ScreenModeNext => None,
                Named::Settings => None,
                Named::SplitScreenToggle => None,
                Named::STBInput => None,
                Named::STBPower => None,
                Named::Subtitle => None,
                Named::Teletext => None,
                Named::VideoModeNext => None,
                Named::Wink => None,
                Named::ZoomToggle => None,
                Named::F1 => Some(Keysym::F1),
                Named::F2 => Some(Keysym::F2),
                Named::F3 => Some(Keysym::F3),
                Named::F4 => Some(Keysym::F4),
                Named::F5 => Some(Keysym::F5),
                Named::F6 => Some(Keysym::F6),
                Named::F7 => Some(Keysym::F7),
                Named::F8 => Some(Keysym::F8),
                Named::F9 => Some(Keysym::F9),
                Named::F10 => Some(Keysym::F10),
                Named::F11 => Some(Keysym::F11),
                Named::F12 => Some(Keysym::F12),
                Named::F13 => Some(Keysym::F13),
                Named::F14 => Some(Keysym::F14),
                Named::F15 => Some(Keysym::F15),
                Named::F16 => Some(Keysym::F16),
                Named::F17 => Some(Keysym::F17),
                Named::F18 => Some(Keysym::F18),
                Named::F19 => Some(Keysym::F19),
                Named::F20 => Some(Keysym::F20),
                Named::F21 => Some(Keysym::F21),
                Named::F22 => Some(Keysym::F22),
                Named::F23 => Some(Keysym::F23),
                Named::F24 => Some(Keysym::F24),
                Named::F25 => Some(Keysym::F25),
                Named::F26 => Some(Keysym::F26),
                Named::F27 => Some(Keysym::F27),
                Named::F28 => Some(Keysym::F28),
                Named::F29 => Some(Keysym::F29),
                Named::F30 => Some(Keysym::F30),
                Named::F31 => Some(Keysym::F31),
                Named::F32 => Some(Keysym::F32),
                Named::F33 => Some(Keysym::F33),
                Named::F34 => Some(Keysym::F34),
                Named::F35 => Some(Keysym::F35),
            },
            iced::keyboard::Key::Character(c) => c.chars().next().map(Keysym::from_char),
            _ => None
        }.into()
    }
}

/// Display a shortcut as a list item
fn shortcut_item(custom: bool, id: BindingId, data: &ShortcutModel) -> Element<ShortcutMessage> {
    #[derive(Copy, Clone, Debug)]
    enum LocalMessage {
        Remove,
        Show,
    }

    let bindings = data
        .bindings
        .iter()
        .take(3)
        .filter(|(_, shortcut)| shortcut.binding.is_set())
        .map(|(_, shortcut)| text::body(shortcut.binding.to_string()).into())
        .collect::<Vec<_>>();

    let shortcuts: Element<LocalMessage> = if bindings.is_empty() {
        text::body(fl!("disabled")).into()
    } else {
        widget::column::with_children(bindings)
            .align_x(Alignment::End)
            .into()
    };

    let modified = if data.modified == 0 {
        None
    } else {
        Some(text::body(fl!("modified", count = data.modified)))
    };

    let control = widget::row::with_capacity(4)
        .push_maybe(modified)
        .push(shortcuts)
        .push(icon::from_name("go-next-symbolic").size(16))
        .push_maybe(custom.then(|| {
            widget::button::icon(icon::from_name("edit-delete-symbolic"))
                .on_press(LocalMessage::Remove)
        }))
        .align_y(Alignment::Center)
        .spacing(8);

    settings::item::builder(&data.description)
        .flex_control(control)
        .spacing(16)
        .apply(widget::container)
        .class(theme::Container::List)
        .apply(widget::button::custom)
        .class(theme::Button::Transparent)
        .on_press(LocalMessage::Show)
        .apply(Element::from)
        .map(move |message| match message {
            LocalMessage::Show => ShortcutMessage::ShowShortcut(id, data.description.clone()),
            LocalMessage::Remove => ShortcutMessage::DeleteShortcut(id),
        })
}
