//! FastH3 Omni generation dialog. The desktop host owns the credential and HTTP work.

use std::sync::mpsc::{Receiver, TryRecvError};

use egui::Context;
use filmcraft_project::BinId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Draft {
    pub open: bool,
    pub prompt: String,
    pub seconds: f64,
    pub aspect: String,
    pub reference_images: Vec<String>,
    pub reference_audios: Vec<String>,
    pub message: String,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            open: false,
            prompt: String::new(),
            seconds: 8.0,
            aspect: "16:9".into(),
            reference_images: vec![],
            reference_audios: vec![],
            message: String::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    pub prompt: String,
    pub seconds: f64,
    pub aspect: String,
    #[serde(default)]
    pub reference_images: Vec<String>,
    #[serde(default)]
    pub reference_audios: Vec<String>,
    /// Empty (or omitted by automation) uses the active project's Reactor media folder.
    #[serde(default)]
    pub output_path: String,
}

fn media_folder(project_path: Option<&str>) -> Result<std::path::PathBuf, String> {
    let project = std::path::Path::new(project_path.ok_or("Save the project before generating a Reactor clip.")?);
    if !project.is_absolute() {
        return Err("Save the project to an absolute path before generating a Reactor clip.".into());
    }
    let parent = project.parent().ok_or("Project folder is unavailable.")?;
    let stem = project.file_stem().filter(|name| !name.is_empty()).ok_or("Project file has no name.")?;
    let mut folder = stem.to_os_string();
    folder.push(" Media");
    Ok(parent.join(folder).join("Reactor"))
}

fn project_output(project_path: Option<&str>) -> Result<String, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let folder = media_folder(project_path)?;
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|error| format!("Could not name Reactor clip: {error}"))?;
    let name = format!("FastH3 Omni-{}-{}.mp4", time.as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed));
    folder.join(name).to_str().map(str::to_owned).ok_or_else(|| "Project media folder is not a valid UTF-8 path.".into())
}

pub enum Event {
    Stage(String),
    Finished(Result<String, String>),
}

pub(crate) struct Running {
    pub receiver: Receiver<Event>,
    pub project_name: String,
    pub project_path: Option<String>,
    pub bin: BinId,
}

pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != "reactor.generate" {
        return None;
    }
    if params.as_object().is_none_or(|p| p.is_empty()) {
        app.ui.reactor.open = true;
        return Some(Ok(json!({"dialog": "reactor"})));
    }
    let request: Request = match serde_json::from_value(params.clone()) {
        Ok(request) => request,
        Err(error) => return Some(Err(format!("Invalid Reactor generation request: {error}"))),
    };
    Some(start(app, request).map(|()| json!({"started": true})))
}

fn start(app: &mut FilmcraftApp, mut request: Request) -> Result<(), String> {
    if app.reactor_running.is_some() {
        return Err("A Reactor generation is already running.".into());
    }
    if request.prompt.trim().is_empty() || request.prompt.len() > 2048 {
        return Err("Enter a prompt of at most 2048 UTF-8 bytes.".into());
    }
    if ![5.875, 8.0, 10.125, 12.25, 14.375].contains(&request.seconds) {
        return Err("Choose a supported FastH3 Omni duration.".into());
    }
    if !["16:9", "9:16", "4:3", "1:1"].contains(&request.aspect.as_str()) {
        return Err("Choose a supported aspect ratio.".into());
    }
    if request.reference_images.len() > 9 || request.reference_audios.len() > 3 || (!request.reference_audios.is_empty() && request.reference_images.is_empty())
    {
        return Err("Use up to 9 images and 3 audio clips; audio requires an image.".into());
    }
    if app.hooks.reactor_generate.is_none() {
        return Err("Reactor generation is available in the desktop app.".into());
    }
    if request.output_path.is_empty() {
        request.output_path = project_output(app.session.path.as_deref())?;
    }
    if !std::path::Path::new(&request.output_path).is_absolute() || !request.output_path.to_ascii_lowercase().ends_with(".mp4") {
        return Err("Choose an absolute MP4 output path.".into());
    }
    let output = std::path::Path::new(&request.output_path);
    if output.exists() {
        return Err("Output file already exists. Choose a new file name.".into());
    }
    let Some(generate) = app.hooks.reactor_generate.as_mut() else {
        return Err("Reactor generation is available in the desktop app.".into());
    };
    app.reactor_running = Some(Running {
        receiver: generate(request),
        project_name: app.session.project.name.clone(),
        project_path: app.session.path.clone(),
        bin: app.import_bin(),
    });
    app.ui.reactor.message = "Starting Reactor generation…".into();
    app.ui.reactor.open = true;
    Ok(())
}

pub fn poll(app: &mut FilmcraftApp, ctx: &Context) {
    let next = app.reactor_running.as_ref().map(|job| job.receiver.try_recv());
    match next {
        Some(Ok(Event::Stage(message))) => app.ui.reactor.message = message,
        Some(Ok(Event::Finished(result))) => {
            let running = app.reactor_running.take();
            match result {
                Err(error) => app.ui.reactor.message = error,
                Ok(path) => {
                    if let Some(running) = running {
                        if app.session.project.name != running.project_name || app.session.path != running.project_path {
                            app.ui.reactor.message = format!("Clip saved to {path}. Import it into the current project when ready.");
                        } else {
                            match app.session.execute("file.import", json!({"paths": [path], "bin": running.bin.0})) {
                                Ok(value) if value["items"].as_array().is_some_and(|items| !items.is_empty()) => {
                                    app.ui.reactor.message = "Reactor clip generated and imported into the Project panel.".into();
                                }
                                Ok(value) => app.ui.reactor.message = format!("Clip saved, but import failed: {}", value["errors"]),
                                Err(error) => app.ui.reactor.message = format!("Clip saved, but import failed: {error}"),
                            }
                        }
                    }
                }
            }
            app.ui.status = app.ui.reactor.message.clone();
        }
        Some(Err(TryRecvError::Disconnected)) => {
            app.reactor_running = None;
            app.ui.reactor.message = "Reactor worker stopped unexpectedly.".into();
            app.ui.status = app.ui.reactor.message.clone();
        }
        _ => {}
    }
    if app.reactor_running.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

pub fn show(app: &mut FilmcraftApp, ctx: &Context) {
    if !app.ui.reactor.open {
        return;
    }
    let mut open = true;
    egui::Window::new(tl!("Generate with Reactor FastH3 Omni")).open(&mut open).collapsible(false).resizable(true).default_width(440.0).show(ctx, |ui| {
        ui.label(tl!("Describe the video and sound"));
        let prompt = ui.add(egui::TextEdit::multiline(&mut app.ui.reactor.prompt).desired_rows(4).desired_width(f32::INFINITY));
        app.auto.add("reactor.prompt", prompt.rect, "Reactor prompt");
        ui.label(format!("{} / 2048 bytes", app.ui.reactor.prompt.len()));
        ui.horizontal(|ui| {
            ui.label(tl!("Duration"));
            egui::ComboBox::from_id_salt("reactor.seconds").selected_text(format!("{} s", app.ui.reactor.seconds)).show_ui(ui, |ui| {
                for seconds in [5.875, 8.0, 10.125, 12.25, 14.375] {
                    let response = ui.selectable_value(&mut app.ui.reactor.seconds, seconds, format!("{seconds} s"));
                    app.auto.add(&format!("reactor.seconds.{seconds}"), response.rect, &format!("{seconds} seconds"));
                }
            });
            ui.label(tl!("Aspect"));
            egui::ComboBox::from_id_salt("reactor.aspect").selected_text(&app.ui.reactor.aspect).show_ui(ui, |ui| {
                for aspect in ["16:9", "9:16", "4:3", "1:1"] {
                    let response = ui.selectable_value(&mut app.ui.reactor.aspect, aspect.into(), aspect);
                    app.auto.add(&format!("reactor.aspect.{aspect}"), response.rect, aspect);
                }
            });
        });
        ui.separator();
        let images = ui.button(format!("{} ({}/9)…", tl!("Reference images"), app.ui.reactor.reference_images.len()));
        app.auto.add("reactor.images", images.rect, "Choose reference images");
        if images.clicked()
            && let Some(pick) = app.hooks.pick_files.as_mut()
        {
            app.ui.reactor.reference_images = pick(&["png", "jpg", "jpeg", "webp"]);
        }
        for (index, path) in app.ui.reactor.reference_images.iter().enumerate() {
            let name = std::path::Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path);
            ui.label(format!("{} {}: {name}", tl!("Picture"), index + 1));
        }
        let audios = ui.button(format!("{} ({}/3)…", tl!("Reference audio"), app.ui.reactor.reference_audios.len()));
        app.auto.add("reactor.audios", audios.rect, "Choose reference audio");
        if audios.clicked()
            && let Some(pick) = app.hooks.pick_files.as_mut()
        {
            app.ui.reactor.reference_audios = pick(&["wav", "mp3", "m4a", "aac", "ogg", "opus", "flac", "webm"]);
        }
        for (index, path) in app.ui.reactor.reference_audios.iter().enumerate() {
            let name = std::path::Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path);
            ui.label(format!("{} {}: {name}", tl!("Audio"), index + 1));
        }
        ui.label(tl!("Use Picture 1 and Audio 1 in the prompt to refer to the first selected files."));
        if !app.ui.reactor.message.is_empty() {
            ui.label(&app.ui.reactor.message);
        }
        ui.separator();
        let saved = match media_folder(app.session.path.as_deref()) {
            Ok(folder) => {
                ui.label(format!("{}: {}", tl!("Location"), folder.display()));
                true
            }
            Err(_) => {
                ui.label(tl!("Save the project before generating. Clips are saved beside the project and imported automatically."));
                let save = ui.button(tl!("Save"));
                app.auto.add("reactor.saveProject", save.rect, "Save project for Reactor clips");
                if save.clicked()
                    && let Err(error) = crate::menus::invoke(app, ctx, "file.saveAs", json!({}))
                {
                    app.ui.reactor.message = error;
                }
                false
            }
        };
        let enabled = saved && app.hooks.reactor_generate.is_some() && app.reactor_running.is_none();
        let generate = ui.add_enabled(enabled, egui::Button::new(tl!("Generate and import")));
        app.auto.add("reactor.start", generate.rect, "Generate and import Reactor clip");
        if generate.clicked() {
            if app.ui.reactor.prompt.trim().is_empty() || app.ui.reactor.prompt.len() > 2048 {
                app.ui.reactor.message = "Enter a prompt of at most 2048 UTF-8 bytes.".into();
            } else {
                let draft = &app.ui.reactor;
                let request = Request {
                    prompt: draft.prompt.clone(),
                    seconds: draft.seconds,
                    aspect: draft.aspect.clone(),
                    reference_images: draft.reference_images.clone(),
                    reference_audios: draft.reference_audios.clone(),
                    output_path: String::new(),
                };
                if let Err(error) = start(app, request) {
                    app.ui.reactor.message = error;
                }
            }
        }
        if app.hooks.reactor_generate.is_none() {
            ui.label(tl!("Generation is available in the desktop app after adding a Reactor API key in Settings."));
        }
    });
    app.ui.reactor.open = open;
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};
    use std::{cell::RefCell, rc::Rc};

    fn app() -> (FilmcraftApp, Rc<RefCell<Vec<Request>>>) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let captured = calls.clone();
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.ui.reactor.open = true;
        app.ui.reactor.prompt = "A paper boat on a lake".into();
        app.hooks.reactor_generate = Some(Box::new(move |request| {
            captured.borrow_mut().push(request);
            let (_, rx) = std::sync::mpsc::channel();
            rx
        }));
        app.hooks.pick_save_as = Some(Box::new(|_, _, _| panic!("generation must not ask for a clip filename")));
        (app, calls)
    }

    #[test]
    fn generate_uses_the_saved_project_without_a_clip_save_dialog() {
        let (mut app, calls) = app();
        let project = std::env::temp_dir().join("My Film.fcproj");
        app.session.path = Some(project.to_str().unwrap().into());
        let expected = project.parent().unwrap().join("My Film Media").join("Reactor");
        let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
        h.run_steps(3);
        h.get_by_label("Generate and import").click();
        h.run_steps(3);
        let calls = calls.borrow();
        assert_eq!(calls.len(), 1);
        let output = std::path::Path::new(&calls[0].output_path);
        assert_eq!(output.parent(), Some(expected.as_path()));
        assert_eq!(output.extension().unwrap(), "mp4");
        assert_ne!(project_output(Some(project.to_str().unwrap())).unwrap(), calls[0].output_path);
    }

    #[test]
    fn unsaved_project_cannot_generate_and_saving_enables_project_storage() {
        let (mut app, calls) = app();
        let dir = std::env::temp_dir().join(format!("fc-reactor-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let project = dir.join("Untitled.fcproj");
        let answer = Rc::new(RefCell::new(None::<String>));
        let chosen = answer.clone();
        app.hooks.pick_save = Some(Box::new(move |_| chosen.borrow().clone()));
        assert!(route(&mut app, "reactor.generate", &json!({"prompt":"lake", "seconds":8, "aspect":"16:9"})).unwrap().is_err());
        assert!(calls.borrow().is_empty());
        let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
        h.run_steps(3);
        h.get_by_label("Save").click();
        h.run_steps(3);
        assert!(h.state().session.path.is_none(), "cancelling leaves the project unsaved");
        assert!(calls.borrow().is_empty());
        *answer.borrow_mut() = Some(project.to_str().unwrap().into());
        h.get_by_label("Save").click();
        h.run_steps(3);
        assert!(project.is_file());
        h.get_by_label("Generate and import").click();
        h.run_steps(3);
        assert_eq!(calls.borrow().len(), 1);
        assert!(std::path::Path::new(&calls.borrow()[0].output_path).starts_with(dir.join("Untitled Media").join("Reactor")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_project_paths_cannot_choose_a_default_destination() {
        assert!(media_folder(None).is_err());
        assert!(media_folder(Some("relative.fcproj")).is_err());
        assert!(media_folder(Some("/")).is_err());
    }
}
