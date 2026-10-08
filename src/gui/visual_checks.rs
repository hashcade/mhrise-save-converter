use super::*;
use gpui_kit::{component::ThemeMode, test::TestWindowExt};
use mhrise_save_converter::{
  format::Platform,
  slots::{SlotInspection, SlotOptions, SlotSummary, inspect_slots},
};
use std::{path::PathBuf, sync::Arc};

fn capture(cx: &mut HeadlessAppContext, handle: AnyWindowHandle, name: &str) {
  let image = cx.capture_screenshot(handle).expect("Metal renderer must be available");
  assert!(image.width() > 0 && image.height() > 0);
  if let Some(directory) = std::env::var_os("MHR_GUI_SCREENSHOTS") {
    image.save(PathBuf::from(directory).join(format!("{name}.png"))).unwrap();
  }
}

pub fn run() {
  let mut cx = HeadlessAppContext::with_platform(
    gpui_kit::platform::current_platform(true).text_system(),
    Arc::new(assets::Assets),
    gpui_kit::platform::current_headless_renderer,
  );
  cx.update(|cx| {
    init(cx);
    cx.set_reduce_motion(true);
  });
  cx.update(|cx| Theme::change(ThemeMode::Light, None, cx));
  let (handle, view) = cx
    .update(|cx| {
      open_window(
        WindowOptions {
          window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: Default::default(),
            size: size(px(820.), px(680.)),
          })),
          focus: false,
          show: false,
          ..Default::default()
        },
        cx,
        |window, cx| cx.new(|cx| GuiView::new(window, cx)),
      )
    })
    .unwrap();

  cx.update_window(handle, |_, window, cx| {
    window.render_frame(cx);
    let left = window.find("source").bounds().left();
    for id in ["reference", "output", "source-id", "target-id"] {
      assert_eq!(window.find(id).bounds().left(), left, "misaligned {id}");
    }
    window.click("source-id", cx);
    window.input("76561198000000000", cx);
  })
  .unwrap();
  cx.update(|cx| assert_eq!(view.read(cx).model.source_steamid64, "76561198000000000"));
  capture(&mut cx, handle, "convert");

  cx.update_window(handle, |_, window, cx| {
    window.click("advanced", cx);
    assert!(window.find("source-curve").visible());
    assert!(window.find("target-curve").visible());
  })
  .unwrap();
  capture(&mut cx, handle, "advanced");

  let fixture = std::env::var_os("MHR_GUI_TEST_SAVE").map(PathBuf::from);
  cx.update_window(handle, |_, window, cx| {
    view.update(cx, |this, cx| {
      let inspection = if let Some(source) = &fixture {
        let id = std::env::var("MHR_GUI_TEST_STEAMID64").expect("fixture needs its SteamID64");
        for (field, value) in
          [(Field::Source, source.display().to_string()), (Field::SourceId, id.clone())]
        {
          this
            .input(field)
            .clone()
            .update(cx, |input, cx| input.set_value(value.clone(), window, cx));
          this.set_field(field, value);
        }
        inspect_slots(
          source,
          SlotOptions { steamid64: Some(id.parse().unwrap()), curve_index: None },
        )
        .unwrap()
      } else {
        SlotInspection {
          platform: Platform::Steam,
          curve_index: Some(67),
          slots: (1..=3)
            .map(|number| SlotSummary {
              number,
              name: (number < 3).then(|| format!("Hunter {number}")),
              hunter_rank: 280,
              master_rank: 122,
              playtime_seconds: 941_646.,
            })
            .collect(),
        }
      };
      this.model.slots_key = Some(this.model.slot_input_key());
      this.model.slots = Some(inspection);
      cx.notify();
    });
    window.within("mode").click(1usize, cx);
    assert!(!view.read(cx).advanced);
    assert!(window.try_find("reference").is_none());
    assert!(!view.read(cx).model.can_save_slots());
    window.click("save", cx);
    assert!(view.read(cx).model.worker.is_none());
    window.click(("up", 0usize), cx);
    window.click(("down", 2usize), cx);
    assert_eq!(view.read(cx).model.slot_order, [1, 2, 3]);
  })
  .unwrap();
  capture(&mut cx, handle, "slots");

  cx.update_window(handle, |_, window, cx| {
    window.click(("down", 0usize), cx);
    assert_eq!(view.read(cx).model.slot_order, [2, 1, 3]);
    assert!(view.read(cx).model.can_save_slots());
    window.click(("down", 1usize), cx);
    assert_eq!(view.read(cx).model.slot_order, [2, 3, 1]);
    window.click(("up", 2usize), cx);
    assert_eq!(view.read(cx).model.slot_order, [2, 1, 3]);
    window.click(("delete", 0usize), cx);
  })
  .unwrap();
  capture(&mut cx, handle, "delete-confirmation");
  cx.update_window(handle, |_, window, cx| {
    window.click("cancel", cx);
    assert_eq!(view.read(cx).model.deleted_slots, [false; 3]);
    window.click(("delete", 0usize), cx);
    window.click("ok", cx);
    assert_eq!(view.read(cx).model.deleted_slots, [false, true, false]);
  })
  .unwrap();

  cx.update_window(handle, |_, window, cx| {
    view.update(cx, |this, cx| {
      this.model.deleted_slots = [false; 3];
      this.model.slot_order = [1, 2, 3];
      cx.notify();
    });
    Theme::change(ThemeMode::Dark, Some(window), cx);
    window.render_frame(cx);
  })
  .unwrap();
  capture(&mut cx, handle, "slots-dark");

  let (sender, receiver) = std::sync::mpsc::channel();
  cx.update_window(handle, |_, window, cx| {
    view.update(cx, |this, cx| {
      this.model.worker = Some(receiver);
      this.after_command(window, cx);
    });
    window.render_frame(cx);
    window.click(("down", 0usize), cx);
    window.within("mode").click(0usize, cx);
    assert_eq!(view.read(cx).model.slot_order, [1, 2, 3]);
    assert_eq!(view.read(cx).model.mode, GuiMode::ManageSlots);
  })
  .unwrap();
  cx.run_until_parked();
  sender.send(super::super::WorkerEvent::Finished(Ok(Vec::new()))).unwrap();
  cx.advance_clock(Duration::from_millis(100));
  cx.run_until_parked();
  cx.update(|cx| {
    assert!(view.read(cx).model.worker.is_none(), "worker polling must finish automatically")
  });

  cx.update_window(handle, |_, window, cx| {
    window.click("source-id", cx);
    window.input("1", cx);
  })
  .unwrap();
  cx.update(|cx| {
    assert!(view.read(cx).model.slots.is_none(), "changed identity invalidates the slot preview")
  });

  let (small, _) = cx
    .update(|cx| {
      open_window(
        WindowOptions {
          window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: Default::default(),
            size: size(px(680.), px(520.)),
          })),
          focus: false,
          show: false,
          ..Default::default()
        },
        cx,
        |window, cx| cx.new(|cx| GuiView::new(window, cx)),
      )
    })
    .unwrap();
  cx.update_window(small, |_, window, cx| {
    window.render_frame(cx);
    let source = window.find("source").bounds();
    assert_eq!(source.left(), window.find("output").bounds().left());
    assert!(source.size.width >= px(300.));
    assert!(window.find("save").visible());
    window.within("platform").click(1usize, cx);
    assert!(window.try_find("target-id").is_none());
  })
  .unwrap();
  capture(&mut cx, small, "compact");
  println!(
    "GUI: real Metal rendering, aligned/compact forms, typing, tabs, advanced options, slot moves, delete confirm/cancel, busy-state guards, automatic worker polling, and stale-preview invalidation passed."
  );
}
