//! Generative Fill's UI: Edit › Generative Fill… (a prompt dialog), the contextual prompt bar
//! under a fresh selection (Photoshop's contextual task bar), the variants section of the
//! Properties panel, and Help › Terms of Use… (the use restrictions every generative model
//! comes with).
//!
//! Every run goes through `ai_model_gate`: with the model files missing, the missing-model
//! dialog opens instead (downloading is the AI Model Manager's job).

use photocraft_doc::LayerId;
use photocraft_engine::genfill_cmds::{ID, VARIANT_ID};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;

const MARK: &str = "__genfill";
/// The remembered Quality (`prefs.dialogs`).
const QUALITY_PREF: &str = "ai.genfill.quality";
const QUALITIES: [&str; 5] = ["auto", "ultra", "fast", "standard", "high"];

/// The Quality the user chose last ("auto" by default).
pub fn saved_quality(app: &PhotocraftApp) -> String {
    app.session.prefs().dialogs.get(QUALITY_PREF).and_then(Value::as_str).filter(|q| QUALITIES.contains(q)).unwrap_or("auto").to_string()
}

fn save_quality(app: &mut PhotocraftApp, q: &str) {
    if saved_quality(app) == q {
        return;
    }
    app.session.edit_prefs(|p| {
        p.dialogs.insert(QUALITY_PREF.into(), json!(q));
    });
    if let Err(e) = crate::prefs_ui::save_preferences(app) {
        log::warn!("couldn't save the Generative Fill quality: {e}");
    }
}
/// The About-dialog field that shows the Terms of Use instead of the tabs.
pub const TERMS_FIELD: &str = "terms";
/// The full licence of the Stable Diffusion weights (CreativeML OpenRAIL-M).
pub const LICENSE_URL: &str = "https://huggingface.co/spaces/CompVis/stable-diffusion-license";

/// The prompt bar's state (not saved).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarState {
    pub prompt: String,
    /// The bar is expanded into the prompt field.
    pub open: bool,
    /// Hidden for this selection (its bounds) until the selection changes.
    pub dismissed: Option<photocraft_geom::Rect>,
}

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key(MARK)
}

/// The menu item without params opens the dialog; with params the command runs as is.
pub fn invoke(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != ID || !params.as_object().is_none_or(|o| o.is_empty()) {
        return None;
    }
    if let Some(Err(why)) = photocraft_engine::commands::find(ID).map(|c| (c.enabled)(&app.session)) {
        return Some(Err(why));
    }
    Some(Ok(json!({"dialog": open(app, &app.ui.genfill.prompt.clone())})))
}

/// Opens the dialog with `prompt` filled in.
pub fn open(app: &mut PhotocraftApp, prompt: &str) -> u64 {
    let mut f = Map::new();
    f.insert(MARK.into(), json!(true));
    f.insert("__command".into(), json!(ID));
    f.insert("__label".into(), json!(tl!("Generative Fill")));
    f.insert("prompt".into(), json!(prompt));
    f.insert("variants".into(), json!("auto"));
    f.insert("quality".into(), json!(saved_quality(app)));
    app.ui.open_dialog(DialogKind::Command, f)
}

/// The command params the dialog's fields stand for.
pub fn params_of(f: &Map<String, Value>) -> Value {
    let prompt = f.get("prompt").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    let quality = f.get("quality").and_then(Value::as_str).filter(|q| QUALITIES.contains(q)).unwrap_or("auto");
    let mut p = json!({"prompt": prompt, "quality": quality});
    // "auto": the tier's default (3 on a GPU, 1 on the light tiers).
    if let Some(v) = f.get("variants").and_then(Value::as_str).and_then(|v| v.parse::<u64>().ok()) {
        p["variants"] = json!(v.clamp(1, 4));
    }
    p
}

pub fn ok_label(f: &Map<String, Value>) -> Option<&'static str> {
    owns(f).then(|| tl!("Generate"))
}

/// The prompt field (shared by the dialog and the bar); true when Enter was pressed.
fn prompt_field(ui: &mut egui::Ui, id: &str, prompt: &mut String, width: f32, focus: bool) -> bool {
    let r = ui.add(
        egui::TextEdit::singleline(prompt).id(egui::Id::unique(id)).char_limit(1000).desired_width(width).hint_text(tl!("Describe what to add (optional)")),
    );
    if focus {
        r.request_focus();
    }
    r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// The line under the prompt: languages, the empty prompt, and the Terms of Use link.
fn notes(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let dim = crate::theme::Tokens::get(ui.ctx()).text_dim;
    ui.label(
        egui::RichText::new(tl!(
            "English, Spanish or Portuguese; empty fills with background. It runs on this device, and generating means you accept the Terms of Use."
        ))
        .color(dim)
        .size(11.0),
    );
    if ui.add(egui::Link::new(egui::RichText::new(tl!("Terms of Use…")).size(11.0))).clicked() {
        open_terms(app);
    }
}

/// Draws the dialog; true when it should close.
pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) -> bool {
    let mut prompt = f.get("prompt").and_then(Value::as_str).unwrap_or_default().to_string();
    let first = !f.contains_key("__focused");
    ui.set_min_width(420.0);
    let enter = prompt_field(ui, "genfill-dialog-prompt", &mut prompt, 420.0, first);
    f.insert("__focused".into(), json!(true));
    f.insert("prompt".into(), json!(prompt));
    ui.add_space(6.0);
    let mut variants = f.get("variants").and_then(Value::as_str).unwrap_or("auto").to_string();
    let mut quality = f.get("quality").and_then(Value::as_str).unwrap_or("auto").to_string();
    ui.horizontal(|ui| {
        ui.label(tl!("Quality:"));
        let opts = [
            ("auto".to_string(), tl!("Automatic")),
            ("ultra".to_string(), tl!("Ultra-light (any device, 280 MB)")),
            ("fast".to_string(), tl!("Fast (1 GB)")),
            ("standard".to_string(), tl!("Standard (GPU, 2.3 GB)")),
            ("high".to_string(), tl!("High (GPU, slow)")),
        ];
        crate::widgets::dropdown(ui, "genfill-quality", "", &mut quality, &opts, 220.0);
        ui.label(tl!("Variants:"));
        let opts: Vec<(String, &str)> =
            std::iter::once(("auto".to_string(), tl!("Automatic"))).chain(["1", "2", "3", "4"].iter().map(|v| ((*v).to_string(), *v))).collect();
        crate::widgets::dropdown(ui, "genfill-variants", tl!("Variants:"), &mut variants, &opts, 90.0);
    });
    f.insert("variants".into(), json!(variants));
    f.insert("quality".into(), json!(quality));
    ui.add_space(6.0);
    notes(app, ui);
    let _ = enter;
    false
}

/// OK is possible with a selection on a pixel layer.
pub fn can_confirm(app: &PhotocraftApp, f: &Map<String, Value>) -> bool {
    !owns(f) || app.session.is_enabled(ID)
}

pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let p = params_of(f);
    save_quality(app, p["quality"].as_str().unwrap_or("auto"));
    app.ui.genfill.prompt = p["prompt"].as_str().unwrap_or_default().to_string();
    run(app, p)
}

fn run(app: &mut PhotocraftApp, params: Value) -> Result<Value, String> {
    crate::ai_model_gate::run(app, ID, params)
}

/// Help › Terms of Use….
pub fn open_terms(app: &mut PhotocraftApp) -> u64 {
    let mut fields = Map::new();
    fields.insert(TERMS_FIELD.into(), json!(true));
    app.ui.open_dialog(DialogKind::About, fields)
}

/// The Terms of Use (in the UI language where we have it: English, Spanish, Portuguese).
pub fn terms_body(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let lang = crate::i18n::current().code();
    let text = if lang.starts_with("es") {
        TERMS_ES
    } else if lang.starts_with("pt") {
        TERMS_PT
    } else {
        TERMS_EN
    };
    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
        ui.set_max_width(520.0);
        for para in text.split("\n\n") {
            ui.label(para);
            ui.add_space(6.0);
        }
    });
    if ui.link(LICENSE_URL).clicked() {
        crate::links::open(app, ui.ctx(), LICENSE_URL);
    }
}

const TERMS_EN: &str = "Capishot Terms of Use: AI features\n\n\
Generative Fill runs Stable Diffusion 1.5 inpainting models (DreamShaper 8 by Lykon; Stable Diffusion by CompVis, Runway and Stability AI), with TCD (MIT) and TAESD (MIT), and on light devices SDXS-512-DreamShaper by IDKiro, on your own device. The models are licensed under the CreativeML OpenRAIL-M licence (SDXS: CreativeML OpenRAIL++-M, with the same restrictions), and your use of them through Capishot is subject to its use restrictions, which are part of these terms and which you agree to follow.\n\n\
You agree not to use the models, or anything made with them:\n\
• in any way that violates any applicable national, federal, state, local or international law or regulation;\n\
• to exploit, harm or attempt to exploit or harm minors in any way;\n\
• to generate or spread verifiably false information intended to harm others;\n\
• to generate or spread personal information that can be used to harm a person;\n\
• to defame, disparage or otherwise harass others;\n\
• for fully automated decision making that adversely affects a person's legal rights or creates or modifies a binding obligation;\n\
• to discriminate against or harm people or groups based on their social behaviour or known or predicted personal or personality characteristics;\n\
• to exploit the vulnerabilities of a group of people based on their age or their social, physical or mental characteristics, in a way that causes or is likely to cause physical or psychological harm;\n\
• to discriminate against people or groups based on legally protected characteristics or categories;\n\
• to provide medical advice or interpret medical results;\n\
• to generate or spread information for the administration of justice, law enforcement, immigration or asylum processes, such as predicting that a person will commit fraud or a crime.\n\n\
Capishot refuses prompts asking for sexual content involving minors or clearly illegal material. You are responsible for what you create and how you use it. The model files were converted by Capishot (16-bit floats, split into parts, the TCD adapter fused at load time); the licensors may update or restrict them. The full licence text:";

const TERMS_ES: &str = "Términos de uso de Capishot: funciones de IA\n\n\
Relleno generativo ejecuta modelos de inpainting de Stable Diffusion 1.5 (DreamShaper 8 de Lykon; Stable Diffusion de CompVis, Runway y Stability AI), con TCD (MIT) y TAESD (MIT), y en equipos ligeros SDXS-512-DreamShaper de IDKiro, en tu propio dispositivo. Los modelos tienen la licencia CreativeML OpenRAIL-M (SDXS: CreativeML OpenRAIL++-M, con las mismas restricciones) y su uso a través de Capishot está sujeto a sus restricciones de uso, que forman parte de estos términos y que aceptas cumplir.\n\n\
Aceptas no usar los modelos, ni nada creado con ellos:\n\
• de ninguna forma que infrinja leyes o normas nacionales, federales, estatales, locales o internacionales;\n\
• para explotar o dañar, o intentar explotar o dañar, a menores de cualquier forma;\n\
• para generar o difundir información falsa verificable con el fin de dañar a otros;\n\
• para generar o difundir información personal que pueda usarse para dañar a alguien;\n\
• para difamar, menospreciar o acosar a otros;\n\
• para decisiones totalmente automatizadas que perjudiquen los derechos legales de una persona o creen o modifiquen obligaciones vinculantes;\n\
• para discriminar o dañar a personas o grupos por su comportamiento social o por características personales conocidas o previstas;\n\
• para aprovechar las vulnerabilidades de un grupo por su edad o sus características sociales, físicas o mentales de un modo que cause o pueda causar daño físico o psicológico;\n\
• para discriminar a personas o grupos por características o categorías protegidas por la ley;\n\
• para dar consejos médicos o interpretar resultados médicos;\n\
• para generar o difundir información destinada a la administración de justicia, la aplicación de la ley o procesos de inmigración o asilo, como predecir que alguien cometerá un fraude o un delito.\n\n\
Capishot rechaza las indicaciones que piden contenido sexual con menores o material claramente ilegal. Eres responsable de lo que creas y de cómo lo usas. Capishot convirtió los archivos del modelo (números de 16 bits, divididos en partes, el adaptador TCD se fusiona al cargarlo); los licenciantes pueden actualizarlos o restringirlos. El texto completo de la licencia (en inglés):";

const TERMS_PT: &str = "Termos de uso do Capishot: recursos de IA\n\n\
O Preenchimento generativo executa modelos de inpainting do Stable Diffusion 1.5 (DreamShaper 8 de Lykon; Stable Diffusion de CompVis, Runway e Stability AI), com TCD (MIT) e TAESD (MIT), e em aparelhos leves SDXS-512-DreamShaper de IDKiro, no seu próprio dispositivo. Os modelos são licenciados sob a licença CreativeML OpenRAIL-M (SDXS: CreativeML OpenRAIL++-M, com as mesmas restrições), e o uso deles pelo Capishot está sujeito às suas restrições de uso, que fazem parte destes termos e que você concorda em seguir.\n\n\
Você concorda em não usar os modelos, nem nada criado com eles:\n\
• de nenhuma forma que viole leis ou regulamentos nacionais, federais, estaduais, locais ou internacionais;\n\
• para explorar ou prejudicar, ou tentar explorar ou prejudicar, menores de qualquer forma;\n\
• para gerar ou divulgar informações comprovadamente falsas com o objetivo de prejudicar outras pessoas;\n\
• para gerar ou divulgar informações pessoais que possam ser usadas para prejudicar alguém;\n\
• para difamar, depreciar ou assediar outras pessoas;\n\
• para decisões totalmente automatizadas que prejudiquem os direitos legais de uma pessoa ou criem ou modifiquem obrigações vinculantes;\n\
• para discriminar ou prejudicar pessoas ou grupos com base em seu comportamento social ou em características pessoais conhecidas ou previstas;\n\
• para explorar as vulnerabilidades de um grupo por sua idade ou suas características sociais, físicas ou mentais de modo que cause ou possa causar dano físico ou psicológico;\n\
• para discriminar pessoas ou grupos com base em características ou categorias protegidas por lei;\n\
• para dar conselhos médicos ou interpretar resultados médicos;\n\
• para gerar ou divulgar informações destinadas à administração da justiça, à aplicação da lei ou a processos de imigração ou asilo, como prever que alguém cometerá fraude ou crime.\n\n\
O Capishot recusa prompts que pedem conteúdo sexual envolvendo menores ou material claramente ilegal. Você é responsável pelo que cria e por como usa. O Capishot converteu os arquivos do modelo (números de 16 bits, divididos em partes, o adaptador TCD é fundido ao carregar); os licenciantes podem atualizá-los ou restringi-los. O texto completo da licença (em inglês):";

// ------------------------------------------------------------------ the prompt bar

/// Whether the bar may show now: the service is there, a selection on a pixel layer, no dialog
/// or job in the way, a selection tool active.
fn bar_visible(app: &PhotocraftApp) -> Option<photocraft_geom::Rect> {
    use crate::state::Tool;
    if app.session.genfill().is_none() || !app.ui.dialogs.is_empty() || app.session.has_jobs() {
        return None;
    }
    if !matches!(
        app.active_tool(),
        Tool::RectMarquee
            | Tool::EllipseMarquee
            | Tool::Lasso
            | Tool::PolygonLasso
            | Tool::MagneticLasso
            | Tool::QuickSelection
            | Tool::ObjectSelection
            | Tool::MagicWand
    ) {
        return None;
    }
    let st = app.session.active()?;
    let sel = st.doc.selection.as_ref()?;
    let b = sel.content_bounds().intersect(&st.doc.bounds());
    if b.is_empty() || app.ui.genfill.dismissed == Some(b) {
        return None;
    }
    Some(b)
}

/// The contextual bar under the selection: "Generative Fill" → prompt field → Generate.
pub fn prompt_bar(app: &mut PhotocraftApp, ctx: &egui::Context) {
    // The touch interface's selection bar offers it instead (touch_ui.rs).
    let bounds = bar_visible(app).filter(|_| !crate::touch_ui::active(app));
    let Some(bounds) = bounds else {
        app.ui.genfill.open = false;
        return;
    };
    let Some(xf) = crate::canvas::ViewXform::active(app) else { return };
    let r = xf.doc_rect(bounds);
    let screen = app.last_canvas_rect;
    let id = egui::Id::unique("genfill-prompt-bar");
    let size = ctx.memory(|m| m.area_rect(id)).map_or(egui::vec2(360.0, 34.0), |a| a.size());
    let (x, y) = (r.center().x - size.x / 2.0, r.bottom() + 10.0);
    let y = if y + size.y > screen.bottom() { (r.top() - size.y - 10.0).max(screen.top()) } else { y };
    let pos = egui::pos2(x.min(screen.right() - size.x).max(screen.left()), y.min(screen.bottom() - size.y).max(screen.top()));
    let mut action: Option<Value> = None;
    let mut dismiss = false;
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::menu(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                if !app.ui.genfill.open {
                    if ui.button(format!("✦ {}", tl!("Generative Fill"))).clicked() {
                        app.ui.genfill.open = true;
                    }
                } else {
                    let mut prompt = std::mem::take(&mut app.ui.genfill.prompt);
                    let enter = prompt_field(ui, "genfill-bar-prompt", &mut prompt, 240.0, false);
                    app.ui.genfill.prompt = prompt;
                    if ui.button(tl!("Generate")).clicked() || enter {
                        action = Some(json!({"prompt": app.ui.genfill.prompt.trim(), "quality": saved_quality(app)}));
                    }
                }
                if ui.small_button("×").on_hover_text(tl!("Close")).clicked() {
                    dismiss = true;
                }
            });
        });
    });
    if dismiss {
        app.ui.genfill.dismissed = Some(bounds);
        app.ui.genfill.open = false;
    }
    if let Some(p) = action
        && let Err(e) = run(app, p)
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

// ------------------------------------------------------------------ Properties

/// The Properties section of a Generative Fill layer: its prompt, the variants (click one to
/// show it) and Generate again.
pub fn properties(app: &mut PhotocraftApp, ui: &mut egui::Ui, layer: LayerId) {
    let Some(v) = app.session.genfill_variants(layer) else { return };
    let (n, selected, prompt) = (v.images.len(), v.selected, v.prompt.clone());
    let mut pick = None;
    let mut again = false;
    if crate::props_layout::section(ui, "genfill", tl!("Generative Fill")) {
        if !prompt.is_empty() {
            ui.label(egui::RichText::new(&prompt).italics());
        }
        ui.horizontal_wrapped(|ui| {
            for i in 0..n {
                if ui.selectable_label(i == selected, format!(" {} ", i + 1)).clicked() && i != selected {
                    pick = Some(i);
                }
            }
        });
        if ui.button(tl!("Generate")).clicked() {
            again = true;
        }
    }
    if let Some(i) = pick
        && let Err(e) = app.run(VARIANT_ID, json!({"index": i, "layer": layer.0}))
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
    if again {
        app.ui.genfill.prompt = prompt;
        let p = app.ui.genfill.prompt.clone();
        open(app, &p);
    }
}
