use super::{GuiMode, GuiModel, GuiTarget};
use gpui_kit::component::{
  ActiveTheme, Disableable, IconName, Sizable, Theme, WindowExt,
  button::{Button, ButtonVariants},
  dialog::DialogButtonProps,
  form::{Field as FormField, Form},
  input::{Input, InputEvent, InputState},
  progress::Progress,
  tab::{Tab, TabBar},
};
use gpui_kit::{prelude::*, *};
use std::time::Duration;

const LABEL_WIDTH: f32 = 160.0;
const BROWSE_WIDTH: f32 = 88.0;
const SLOT_ROW_HEIGHT: f32 = 58.0;

#[derive(Clone, Copy)]
enum Field {
  Source,
  Reference,
  Output,
  SourceId,
  TargetId,
  SourceCurve,
  TargetCurve,
}

impl Field {
  fn id(self) -> &'static str {
    match self {
      Self::Source => "source",
      Self::Reference => "reference",
      Self::Output => "output",
      Self::SourceId => "source-id",
      Self::TargetId => "target-id",
      Self::SourceCurve => "source-curve",
      Self::TargetCurve => "target-curve",
    }
  }
}

pub(super) struct GuiView {
  model: GuiModel,
  inputs: [Entity<InputState>; 7],
  advanced: bool,
  picking_folder: bool,
  _subscriptions: Vec<Subscription>,
  worker_task: Option<Task<()>>,
}

impl GuiView {
  fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
    let hints = [
      "Existing save folder",
      "Existing save folder for the target platform",
      "Folder for the new save",
      "SteamID64 of the source account",
      "SteamID64 of the target account",
      "Detected automatically",
      "Detected from target save",
    ];
    let inputs = hints.map(|hint| cx.new(|cx| InputState::new(window, cx).placeholder(hint)));
    let fields = [
      Field::Source,
      Field::Reference,
      Field::Output,
      Field::SourceId,
      Field::TargetId,
      Field::SourceCurve,
      Field::TargetCurve,
    ];
    let mut subscriptions: Vec<Subscription> = fields
      .into_iter()
      .zip(&inputs)
      .map(|(field, input)| {
        cx.subscribe_in(input, window, move |this, input, event, _, cx| {
          if matches!(event, InputEvent::Change) {
            this.set_field(field, input.read(cx).value().to_string());
            cx.notify();
          }
        })
      })
      .collect();
    subscriptions.push(window.observe_window_appearance(|window, cx| {
      Theme::sync_system_appearance(Some(window), cx);
    }));
    Self {
      model: GuiModel::default(),
      inputs,
      advanced: false,
      picking_folder: false,
      _subscriptions: subscriptions,
      worker_task: None,
    }
  }

  fn busy(&self) -> bool {
    self.model.worker.is_some() || self.picking_folder
  }

  fn input(&self, field: Field) -> &Entity<InputState> {
    &self.inputs[field as usize]
  }

  fn set_field(&mut self, field: Field, value: String) {
    match field {
      Field::Source => self.model.source = value,
      Field::Reference => self.model.target_reference = value,
      Field::Output => self.model.output = value,
      Field::SourceId => self.model.source_steamid64 = value,
      Field::TargetId => self.model.target_steamid64 = value,
      Field::SourceCurve => self.model.source_curve_index = value,
      Field::TargetCurve => self.model.target_curve_index = value,
    }
    self.model.refresh_source_platform();
    self.model.invalidate_slots();
    self.model.preflight = None;
  }

  fn browse(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
    if self.busy() {
      return;
    }
    self.picking_folder = true;
    let slot_output = matches!(field, Field::Output) && self.model.mode == GuiMode::ManageSlots;
    cx.spawn_in(window, async move |view, cx| {
      let selected = rfd::AsyncFileDialog::new().pick_folder().await;
      let _ = cx.update(|window, cx| {
        let _ = view.update(cx, |this, cx| {
          this.picking_folder = false;
          if let Some(selected) = selected {
            let path = if slot_output {
              selected.path().join("swapped-save")
            } else {
              selected.path().to_path_buf()
            };
            let value = path.display().to_string();
            this
              .input(field)
              .clone()
              .update(cx, |input, cx| input.set_value(value.clone(), window, cx));
            this.set_field(field, value);
          }
          cx.notify();
        });
      });
    })
    .detach();
    cx.notify();
  }

  fn after_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.open_confirmations(window, cx);
    if self.model.worker.is_some() {
      self.worker_task = Some(cx.spawn(async move |view, cx| {
        loop {
          cx.background_executor().timer(Duration::from_millis(100)).await;
          let running = view.update(cx, |this, cx| {
            this.model.poll_worker();
            cx.notify();
            this.model.worker.is_some()
          });
          if !matches!(running, Ok(true)) {
            break;
          }
        }
      }));
    }
    cx.notify();
  }

  fn open_confirmations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(pending) = self.model.pending_conversion.take() {
      let view = cx.weak_entity();
      window.open_alert_dialog(cx, move |dialog, _, _| {
        let pending = pending.clone();
        let view = view.clone();
        dialog
          .title("Overwrite existing files?")
          .width(px(460.))
          .button_props(
            DialogButtonProps::default().show_cancel(true).ok_text("Overwrite and convert"),
          )
          .child(
            div()
              .flex()
              .flex_col()
              .gap_2()
              .child(format!("{} file(s) already exist in:", pending.conflicts.len()))
              .child(pending.output.display().to_string())
              .child(div().id("conflicting-files").max_h(px(160.)).overflow_y_scroll().children(
                pending.conflicts.iter().map(|path| {
                  div().child(path.file_name().unwrap_or_default().to_string_lossy().into_owned())
                }),
              )),
          )
          .on_ok(move |_, window, cx| {
            let _ = view.update(cx, |this, cx| {
              this.model.start_conversion(Some(pending.clone()));
              let view = cx.weak_entity();
              window.on_next_frame(move |window, cx| {
                let _ = view.update(cx, |this, cx| this.after_command(window, cx));
              });
              cx.notify();
            });
            true
          })
      });
    }
    if let Some(pending) = self.model.pending_slot_deletion.take() {
      let view = cx.weak_entity();
      window.open_alert_dialog(cx, move |dialog, _, _| {
        let pending = pending.clone();
        let view = view.clone();
        dialog.title("Delete character?").width(px(400.))
          .button_props(DialogButtonProps::default().show_cancel(true).ok_text("Delete"))
          .child(format!("Slot {} · {}", pending.index + 1, pending.name))
          .child("The character and its album will be excluded when you save. The source stays unchanged.")
          .on_ok(move |_, _, cx| {
            let _ = view.update(cx, |this, cx| {
              this.model.confirm_slot_deletion(pending.clone());
              cx.notify();
            });
            true
          })
      });
    }
  }

  fn field(
    &self,
    field: Field,
    label: &'static str,
    browse: bool,
    cx: &mut Context<Self>,
  ) -> FormField {
    let busy = self.busy();
    FormField::new()
      .label(label)
      .required(matches!(field, Field::Source | Field::Output | Field::SourceId | Field::TargetId))
      .items_center()
      .child(
        div()
          .flex()
          .items_center()
          .gap_3()
          .w_full()
          .child(
            div()
              .flex_1()
              .min_w_0()
              .child(Input::new(self.input(field)).id(field.id()).disabled(busy)),
          )
          .child(div().w(px(BROWSE_WIDTH)).flex_shrink_0().when(browse, |this| {
            this.child(
              Button::new(("browse", field as usize))
                .label("Browse…")
                .outline()
                .disabled(busy)
                .w_full()
                .on_click(cx.listener(move |this, _, window, cx| this.browse(field, window, cx))),
            )
          })),
      )
  }

  fn slot_table(&self, cx: &mut Context<Self>) -> Stateful<Div> {
    let busy = self.busy();
    let mut table = div()
      .id("slot-table")
      .flex()
      .flex_col()
      .w_full()
      .border_1()
      .border_color(cx.theme().border)
      .rounded_lg()
      .overflow_hidden()
      .child(
        div()
          .flex()
          .items_center()
          .h(px(38.))
          .px_4()
          .bg(cx.theme().muted)
          .text_color(cx.theme().muted_foreground)
          .child(div().w(px(64.)).child("Slot"))
          .child(div().flex_1().child("Character"))
          .child(div().w(px(140.)).child("Progress"))
          .child(div().w(px(148.)).child("Actions")),
      );
    for index in 0..3 {
      let slot = self.model.slot_character(index);
      let occupied = slot.is_some();
      let name = slot.and_then(|slot| slot.name.clone()).unwrap_or_else(|| "Empty".to_owned());
      let mut progress = div()
        .w(px(140.))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .justify_center()
        .text_color(cx.theme().muted_foreground);
      if let Some(slot) = slot {
        progress = progress
          .child(format!("HR {} · MR {}", slot.hunter_rank, slot.master_rank))
          .child(div().text_xs().child(slot.playtime()));
      } else {
        progress = progress.child("Empty slot");
      }
      table = table.child(
        div()
          .id(("slot-row", index))
          .flex()
          .items_center()
          .h(px(SLOT_ROW_HEIGHT))
          .px_4()
          .border_t_1()
          .border_color(cx.theme().border)
          .child(
            div()
              .id(("slot-number", index))
              .w(px(64.))
              .flex_shrink_0()
              .child(format!("Slot {}", index + 1)),
          )
          .child(
            div()
              .id(("slot-name", index))
              .flex_1()
              .min_w_0()
              .truncate()
              .text_color(if occupied {
                cx.theme().foreground
              } else {
                cx.theme().muted_foreground
              })
              .child(name),
          )
          .child(progress)
          .child(
            div()
              .flex()
              .items_center()
              .gap_1()
              .w(px(148.))
              .flex_shrink_0()
              .child(
                Button::new(("up", index))
                  .outline()
                  .small()
                  .icon(IconName::ArrowUp)
                  .tooltip("Move up")
                  .disabled(busy || !occupied || index == 0)
                  .on_click(cx.listener(move |this, _, _, cx| {
                    this.model.move_slot(index, true);
                    cx.notify();
                  })),
              )
              .child(
                Button::new(("down", index))
                  .outline()
                  .small()
                  .icon(IconName::ArrowDown)
                  .tooltip("Move down")
                  .disabled(busy || !occupied || index == 2)
                  .on_click(cx.listener(move |this, _, _, cx| {
                    this.model.move_slot(index, false);
                    cx.notify();
                  })),
              )
              .child(
                Button::new(("delete", index))
                  .ghost()
                  .small()
                  .label("Delete")
                  .disabled(busy || !occupied)
                  .on_click(cx.listener(move |this, _, window, cx| {
                    this.model.request_slot_deletion(index);
                    this.after_command(window, cx);
                  })),
              ),
          ),
      );
    }
    table
  }

  fn status(&self, cx: &mut Context<Self>) -> Div {
    let mut status = div().flex().flex_col().gap_2();
    if !self.model.status.is_empty() {
      status = status.child(self.model.status.clone());
    }
    if let Some(progress) = &self.model.progress {
      status = status
        .child(
          Progress::new("save-progress")
            .value(100. * progress.completed as f32 / progress.total.max(1) as f32),
        )
        .child(format!(
          "{} / {} · {}",
          progress.completed,
          progress.total,
          progress.current_file.display()
        ));
    }
    if let Some(report) = &self.model.preflight {
      status = status
        .child(div().text_color(cx.theme().muted_foreground).child(format!(
          "Files: {} ({} core, {} album/photo)",
          report.file_count, report.core_file_count, report.auxiliary_file_count
        )))
        .children(report.errors().map(|error| div().child(format!("Error: {error}"))))
        .children(report.warnings().map(|warning| div().child(format!("Warning: {warning}"))));
    }
    status
  }
}

impl Render for GuiView {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let busy = self.busy();
    let managing = self.model.mode == GuiMode::ManageSlots;
    let mut form = Form::horizontal()
      .label_width(px(LABEL_WIDTH))
      .gap_y(px(16.))
      .child(self.field(Field::Source, "Source save", true, cx));
    if !managing {
      form = form.child(self.field(Field::Reference, "Target save", true, cx));
    }
    form = form.child(self.field(Field::Output, "Output folder", true, cx));
    if !managing {
      form = form.child(
        FormField::new().label("Convert to").items_center().child(
          div().flex().items_center().child(
            TabBar::new("platform")
              .segmented()
              .selected_index(usize::from(self.model.target == GuiTarget::Switch))
              .child(Tab::new().label("Steam").disabled(busy))
              .child(Tab::new().label("Nintendo Switch").disabled(busy))
              .on_click(cx.listener(|this, index, _, cx| {
                if !this.busy() {
                  this.model.target =
                    if *index == 0 { GuiTarget::Steam } else { GuiTarget::Switch };
                  this.model.preflight = None;
                  cx.notify();
                }
              })),
          ),
        ),
      );
    }
    if !self.model.source_is_switch() {
      form = form.child(self.field(
        Field::SourceId,
        if managing { "Current SteamID64" } else { "Source SteamID64" },
        false,
        cx,
      ));
    }
    if !managing && self.model.target == GuiTarget::Steam {
      form = form.child(self.field(Field::TargetId, "Target SteamID64", false, cx));
    }
    if !self.model.source_is_switch() || (!managing && self.model.target == GuiTarget::Steam) {
      form = form.child(
        FormField::new().label_indent(false).child(
          div().flex().items_center().child(
            Button::new("advanced")
              .ghost()
              .small()
              .icon(if self.advanced { IconName::ChevronDown } else { IconName::ChevronRight })
              .label("Advanced options")
              .disabled(busy)
              .on_click(cx.listener(|this, _, _, cx| {
                this.advanced = !this.advanced;
                cx.notify();
              })),
          ),
        ),
      );
      if self.advanced {
        if !self.model.source_is_switch() {
          form = form.child(self.field(Field::SourceCurve, "Source Curve Index", false, cx));
        }
        if !managing && self.model.target == GuiTarget::Steam {
          form = form.child(self.field(Field::TargetCurve, "Target Curve Index", false, cx));
        }
      }
    }
    let mut content = div().flex().flex_col().gap_4().w_full().child(form);
    if managing {
      content = content.child(div().flex().justify_start().child(
        Button::new("read-save").outline().label("Read save").disabled(busy).on_click(cx.listener(
          |this, _, window, cx| {
            this.model.load_slots();
            this.after_command(window, cx);
          },
        )),
      ));
      if self.model.slots.is_some() {
        content = content.child(self.slot_table(cx));
      }
    }
    let output_available =
      !busy && self.model.output_directory.as_deref().is_some_and(|path| path.is_dir());
    content = content.child(
      div()
        .flex()
        .items_center()
        .justify_between()
        .mt_2()
        .child(div().when(output_available, |this| {
          this.child(Button::new("open-output").outline().label("Open output folder").on_click(
            cx.listener(|this, _, _, cx| {
              this.model.open_output_directory();
              cx.notify();
            }),
          ))
        }))
        .child(
          Button::new("save")
            .primary()
            .label(if managing { "Save slots" } else { "Convert save" })
            .disabled(busy || (managing && !self.model.can_save_slots()))
            .on_click(cx.listener(|this, _, window, cx| {
              if this.model.mode == GuiMode::ManageSlots {
                this.model.save_slot_order();
              } else {
                this.model.start_conversion(None);
              }
              this.after_command(window, cx);
            })),
        ),
    );
    content = content.child(self.status(cx));

    div()
      .id("save-app")
      .size_full()
      .flex()
      .flex_col()
      .bg(cx.theme().background)
      .text_color(cx.theme().foreground)
      .text_sm()
      .p(px(24.))
      .gap_6()
      .child(
        TabBar::new("mode")
          .underline()
          .selected_index(usize::from(managing))
          .child(Tab::new().label("Convert save").disabled(busy))
          .child(Tab::new().label("Manage slots").disabled(busy))
          .on_click(cx.listener(|this, index, _, cx| {
            if !this.busy() {
              this.model.mode = if *index == 0 { GuiMode::Convert } else { GuiMode::ManageSlots };
              this.advanced = false;
              cx.notify();
            }
          })),
      )
      .child(div().id("save-content").flex_1().min_h_0().overflow_y_scroll().child(content))
  }
}

fn window_options(cx: &App) -> WindowOptions {
  WindowOptions {
    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
      None,
      size(px(820.), px(680.)),
      cx,
    ))),
    window_min_size: Some(size(px(680.), px(520.))),
    titlebar: Some(TitlebarOptions {
      title: Some("Monster Hunter Rise Save Converter".into()),
      ..Default::default()
    }),
    ..Default::default()
  }
}

pub fn run() {
  application().with_assets(assets::Assets).run(|cx| {
    init(cx);
    Theme::sync_system_appearance(None, cx);
    cx.on_window_closed(|cx, _| {
      if cx.windows().is_empty() {
        cx.quit();
      }
    })
    .detach();
    open_window(window_options(cx), cx, |window, cx| cx.new(|cx| GuiView::new(window, cx)))
      .expect("Failed to open save converter window");
    cx.activate(true);
  });
}

#[cfg(feature = "gui-test")]
#[allow(dead_code)]
pub fn run_visual_checks() {
  #[cfg(target_os = "macos")]
  visual_checks::run();
  #[cfg(not(target_os = "macos"))]
  println!("GUI rendering checks skipped: this suite currently runs on macOS only.");
}
#[cfg(all(feature = "gui-test", target_os = "macos"))]
#[allow(dead_code)]
#[path = "visual_checks.rs"]
mod visual_checks;
