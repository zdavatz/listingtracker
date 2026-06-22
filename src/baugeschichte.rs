// Render the "Baugeschichte" (building history) of Erica's house in Ermioni
// from the synced WhatsApp data in erica-house/messages.json into a titled
// photo-documentation PDF — German and Greek.
//
// Pure Rust, no Chrome: the PDF is built directly with `genpdf` (which writes
// PDF via printpdf), embedding the DejaVu Sans font family (covers Latin +
// Greek + umlauts) and the photos. Replaces the earlier HTML+Chrome / Node
// pipeline.
//
//   cargo run --release --bin baugeschichte                 # both DE + EL
//   cargo run --release --bin baugeschichte -- --lang de    # German only
//   cargo run --release --bin baugeschichte -- --lang el    # Greek only
//
// Font dir override: $FONT_DIR (default /usr/share/fonts/dejavu).

use std::env;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use genpdf::elements::{Break, Image, PageBreak, Paragraph};
use genpdf::style::{Color, Style};
use genpdf::Alignment;
use serde_json::Value;

const DIR: &str = "erica-house";
const DEFAULT_FONT_DIR: &str = "/usr/share/fonts/dejavu";

// Printable area inside the page margins, used to scale photos via DPI so they
// never overflow the content box (portrait photos cap on height, landscape on
// width).
const MAX_IMG_W_MM: f64 = 168.0;
const MAX_IMG_H_MM: f64 = 180.0;

// printpdf 0.3 (used by genpdf) embeds photos as a raster, so PDF size scales
// with pixel count, not the source JPEG bytes. Downscale to ~150 dpi at full
// display size to keep the file shareable while staying print-sharp.
const TARGET_LONG_PX: u32 = 1100;

// Location (Google Maps) and the goutos.gr listing for the house. genpdf 0.2
// has no hyperlink support, so these render as selectable URL text on the
// cover; most PDF viewers auto-linkify them.
const MAPS_URL: &str = "https://maps.app.goo.gl/gXk1sFUhneHTnthr5";
const LISTING_URL: &str = "https://www.goutos.gr/en-US/property/500193";

// The two cover URL lines are the ONLY text drawn at this size on page 1, which
// is how add_cover_links() locates them in the content stream to overlay
// clickable link annotations. Keep layout and finder in sync via this const.
const COVER_LINK_FONT_SIZE: u8 = 10;
const A4_WIDTH_PT: f64 = 595.276;

// Palette echoing the old CSS design.
const GOLD: Color = Color::Rgb(0xa0, 0x8b, 0x6a);
const BROWN: Color = Color::Rgb(0x6b, 0x5a, 0x44);
const INK: Color = Color::Rgb(0x1f, 0x1a, 0x16);

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    De,
    El,
}

impl Lang {
    fn slug_suffix(self) -> &'static str {
        match self {
            Lang::De => "",    // baugeschichte.pdf
            Lang::El => "_gr", // baugeschichte_gr.pdf
        }
    }
    fn kicker(self) -> &'static str {
        match self {
            Lang::De => "BAUGESCHICHTE IN BILDERN",
            Lang::El => "Η ΙΣΤΟΡΙΑ ΤΗΣ ΚΑΤΑΣΚΕΥΗΣ ΣΕ ΕΙΚΟΝΕΣ",
        }
    }
    fn house_of(self) -> &'static str {
        match self {
            Lang::De => "Das Haus von",
            Lang::El => "Το σπίτι της",
        }
    }
    fn place(self) -> &'static str {
        match self {
            Lang::De => "Ermioni, Griechenland",
            Lang::El => "Ερμιόνη, Ελλάδα",
        }
    }
    fn lead(self) -> &'static str {
        match self {
            Lang::De => "Vom Kauf im Jahr 1990 bis zu den späteren Umbauten — eine fotografische Dokumentation der Veränderungen über die Jahre.",
            Lang::El => "Από την αγορά το 1990 έως τις μετέπειτα μετατροπές — μια φωτογραφική τεκμηρίωση των αλλαγών στο πέρασμα των χρόνων.",
        }
    }
    fn intro_heading(self) -> &'static str {
        match self {
            Lang::De => "Zur Geschichte des Hauses",
            Lang::El => "Η ιστορία του σπιτιού",
        }
    }
    fn plate_word(self) -> &'static str {
        match self {
            Lang::De => "BILD",
            Lang::El => "ΕΙΚΟΝΑ",
        }
    }
    fn location_label(self) -> &'static str {
        match self {
            Lang::De => "Ort",
            Lang::El => "Τοποθεσία",
        }
    }
    fn listing_label(self) -> &'static str {
        match self {
            Lang::De => "Inserat",
            Lang::El => "Αγγελία",
        }
    }
    fn footer(self, name: &str) -> String {
        match self {
            Lang::De => format!(
                "Zusammengestellt aus den Bildern und Beschreibungen von {} · Haus in Ermioni",
                name
            ),
            Lang::El => format!(
                "Συντάχθηκε από τις εικόνες και τις περιγραφές της {} · Σπίτι στην Ερμιόνη",
                name
            ),
        }
    }
}

// German original (trimmed) -> Greek. Mirrors the TR map in pdf-generator-gr.mjs.
const TR: &[(&str, &str)] = &[
    ("Guten Tag lieber Zeno\nGerne treffe ich dich zu einem Gespräch, das der Verkauf von meinem Häuschen in Ermioni betrifft. \nBitte mach mir einen Vorschlag. \n           Grüsse aus Höngg\n           Erika",
     "Καλημέρα αγαπητέ Ζένο\nΜε χαρά θα σε συναντήσω για μια συζήτηση που αφορά την πώληση του σπιτιού μου στην Ερμιόνη. \nΣε παρακαλώ κάνε μου μια πρόταση. \n           Χαιρετίσματα από το Höngg\n           Έρικα"),
    ("Kommt noch einiges mehr. Auch von der Gasse und Wohnzimmer vor 5-6 Jahren.",
     "Θα ακολουθήσουν κι άλλα. Επίσης από το σοκάκι και το σαλόνι πριν από 5-6 χρόνια."),
    ("Muss nun einen Unterbruch machen.",
     "Πρέπει τώρα να κάνω ένα διάλειμμα."),
    ("Entschuldige, habe beim durchlesen einige Flüchtigkeitsfehler entdeckt. \n            Am Nachmittag geht es weiter.",
     "Συγγνώμη, διαβάζοντας ξανά εντόπισα μερικά λαθάκια απροσεξίας. \n            Το απόγευμα συνεχίζουμε."),
    ("Zu meinem anderen Nachbar mit kl. Haus. Hauseingang zur Strasse. \nDas Haus hat er geerbt.\nAls ehemaliger Schiffsingenieur war sein Wunsch selbstständig einige Reparaturen am Haus zu machen. \nWir konnten uns gut verständigen trotz unterschiedlicher Sprache.",
     "Σχετικά με τον άλλο μου γείτονα με το μικρό σπίτι. Η είσοδος του σπιτιού προς τον δρόμο. \nΤο σπίτι το κληρονόμησε.\nΩς πρώην μηχανικός πλοίων, επιθυμία του ήταν να κάνει μόνος του κάποιες επισκευές στο σπίτι. \nΜπορούσαμε να συνεννοηθούμε καλά παρά τη διαφορετική γλώσσα."),
    ("Anfangs Jahr 1991 begleitete mich Hans nach Ermioni mit Material für Vermessungen vorzunehmen und einen Plan zuerstellen für die Arbeiter. Ich fotografierte drauf los. Notierte alle Veränderungen, die gemacht werden mussten. Die erste Material Einkäufe, die ich von der Schweiz transportieren wollte.",
     "Στις αρχές του 1991 με συνόδευσε ο Χανς στην Ερμιόνη με υλικά, για να κάνουμε μετρήσεις και να ετοιμάσουμε ένα σχέδιο για τους εργάτες. Φωτογράφιζα ασταμάτητα. Σημείωνα όλες τις αλλαγές που έπρεπε να γίνουν. Τις πρώτες αγορές υλικών που ήθελα να μεταφέρω από την Ελβετία."),
    ("1990 H.Besichtigung und gekauft.\nRe. Rotestor H.Eingang.\nWäsche der Nachbarn.",
     "1990 Επίσκεψη και αγορά του σπιτιού.\nΔεξιά κόκκινη πύλη, είσοδος του σπιτιού.\nΜπουγάδα των γειτόνων."),
    ("Abbruch von 2 kleinen Balkonen für Chminéeholz.\nLi. S. hinten Dusche m. WC. Re. Eingangstor.\nHeute geschlossener Duschraum.",
     "Κατεδάφιση 2 μικρών μπαλκονιών για ξύλα του τζακιού.\nΑριστερά πίσω ντους με WC. Δεξιά η πύλη εισόδου.\nΣήμερα κλειστός χώρος ντους."),
    ("Neuer TerrassenAufbau.\nNeuer doppelter Dach-\nIsolierung.",
     "Νέα κατασκευή της ταράτσας.\nΝέα διπλή μόνωση\nοροφής."),
    ("Anfertigung von zwei BalkonSeitenwände zu den Nachbarn.",
     "Κατασκευή δύο πλαϊνών τοίχων μπαλκονιού προς τους γείτονες."),
    ("Alte Küche mit einem kleinen Fenster.",
     "Παλιά κουζίνα με ένα μικρό παράθυρο."),
    ("Hinter der Küche und Duschraum kam eine kurze Treppe vom Nachbarhaus in m. Haus.\nAusgeräumt, umfunktioniert zu einem geschlossenen Putzschrank.",
     "Πίσω από την κουζίνα και τον χώρο ντους υπήρχε μια κοντή σκάλα από το γειτονικό σπίτι προς το δικό μου.\nΑδειάστηκε και μετατράπηκε σε κλειστή ντουλάπα καθαριστικών."),
    ("Durch einen kleinen Durchgang mit 2 grossen Bollensteine gelangte man\nIn Chminéeraum= Wohnraum.",
     "Μέσα από ένα μικρό πέρασμα με 2 μεγάλες πέτρες έφτανε κανείς\nστον χώρο του τζακιού = σαλόνι."),
    ("Fenster zur zur hinteren Gasse.",
     "Παράθυρο προς το πίσω σοκάκι."),
    ("Fenster im Wohnraum, Chminée, Sicht in einem Schacht zum Nachbar. Der mit grossen unvertigen grossen Haus.\nAnstelle vom Schacht wurde ein Kasten gebaut für Büroarbeiten.",
     "Παράθυρο στο σαλόνι, τζάκι, θέα μέσα από ένα φρεάτιο προς τον γείτονα — αυτόν με το μεγάλο ημιτελές σπίτι.\nΣτη θέση του φρεατίου κατασκευάστηκε ένα ερμάριο για γραφειακή εργασία."),
    ("Aufstieg in 1. Stock.",
     "Άνοδος στον 1ο όροφο."),
    ("Oberhalb der Treppe befindet sich ein AtrappenFenster. Auf passen beim Aufstieg.\n„Dunkelheit“. Böden wurden im ganzen erneuert.",
     "Πάνω από τη σκάλα υπάρχει ένα ψεύτικο παράθυρο. Προσοχή κατά την άνοδο.\n«Σκοτάδι». Τα δάπεδα ανανεώθηκαν εξ ολοκλήρου."),
    ("Fenster vom Wohnraum zur hinteren Strasse.",
     "Παράθυρο του σαλονιού προς τον πίσω δρόμο."),
    ("Ende Jahr 1990 war es soweit.",
     "Στα τέλη του 1990 ήρθε η στιγμή."),
    ("Jürg organisierte mir einen Übersetzer der mich zur Anwältin  mitnahm.",
     "Ο Γιούργκ μου κανόνισε έναν μεταφραστή που με πήγε στη δικηγόρο."),
    ("Beim Unterschreiben.  Somit hatte ich einen HausGötti.",
     "Κατά την υπογραφή. Έτσι απέκτησα έναν «νονό του σπιτιού»."),
    ("Beim Unterscheiben. Das Haus ist gekauft.",
     "Κατά την υπογραφή. Το σπίτι αγοράστηκε."),
    ("Hans erster Arbeitstag.",
     "Η πρώτη εργάσιμη μέρα του Χανς."),
    ("Hans beim Vermessen",
     "Ο Χανς κατά τη μέτρηση."),
    ("Hans macht erste Bekanntschaft mit Matina Notara.",
     "Ο Χανς γνωρίζεται για πρώτη φορά με τη Ματίνα Νοταρά."),
];

fn translate(lang: Lang, s: &str) -> String {
    if lang == Lang::De {
        return s.to_string();
    }
    let key = s.trim();
    for (de, el) in TR {
        if *de == key {
            return (*el).to_string();
        }
    }
    s.to_string() // fall through unchanged so nothing is silently lost
}

fn is_image(file: &str) -> bool {
    let f = file.to_lowercase();
    [".jpg", ".jpeg", ".png", ".webp", ".gif"]
        .iter()
        .any(|e| f.ends_with(e))
}

/// Push each `\n`-separated line of `text` as its own aligned, styled paragraph
/// so multi-line captions/notes keep their line breaks.
fn push_lines(doc: &mut genpdf::Document, text: &str, style: Style, align: Alignment) {
    for line in text.split('\n') {
        let mut p = Paragraph::default();
        p.push_styled(line.to_string(), style);
        doc.push(p.aligned(align));
    }
}

/// DPI that makes a photo of the given pixel size fit inside the printable box
/// (cap on the more constraining dimension), so genpdf scales it to fit.
fn dpi_for(w: u32, h: u32) -> f64 {
    let dpi_w = w as f64 * 25.4 / MAX_IMG_W_MM;
    let dpi_h = h as f64 * 25.4 / MAX_IMG_H_MM;
    dpi_w.max(dpi_h).max(72.0)
}

/// Load a photo, downscale its long edge to TARGET_LONG_PX (Lanczos3), and
/// re-encode to JPEG bytes. Returns the bytes plus the final pixel size. Going
/// through JPEG bytes sidesteps the image-crate version skew between this
/// project (0.25) and genpdf (0.23): genpdf decodes the bytes itself.
fn scaled_jpeg(path: &Path) -> Result<(Vec<u8>, u32, u32)> {
    let img = image::open(path).map_err(|e| anyhow!("open {}: {}", path.display(), e))?;
    let (w, h) = (img.width(), img.height());
    let long = w.max(h);
    let img = if long > TARGET_LONG_PX {
        let s = TARGET_LONG_PX as f32 / long as f32;
        img.resize(
            (w as f32 * s).round() as u32,
            (h as f32 * s).round() as u32,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img
    };
    let (fw, fh) = (img.width(), img.height());
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Jpeg)
        .map_err(|e| anyhow!("encode {}: {}", path.display(), e))?;
    Ok((buf, fw, fh))
}

fn load_font_family(font_dir: &str) -> Result<genpdf::fonts::FontFamily<genpdf::fonts::FontData>> {
    let load = |file: &str| -> Result<genpdf::fonts::FontData> {
        let path = Path::new(font_dir).join(file);
        let data = std::fs::read(&path).map_err(|e| anyhow!("read font {}: {}", path.display(), e))?;
        genpdf::fonts::FontData::new(data, None).map_err(|e| anyhow!("parse font {}: {}", file, e))
    };
    Ok(genpdf::fonts::FontFamily {
        regular: load("DejaVuSans.ttf")?,
        bold: load("DejaVuSans-Bold.ttf")?,
        italic: load("DejaVuSans-Oblique.ttf")?,
        bold_italic: load("DejaVuSans-BoldOblique.ttf")?,
    })
}

fn render(lang: Lang, name: &str, messages: &[Value], font_dir: &str) -> Result<PathBuf> {
    let family = load_font_family(font_dir)?;
    let mut doc = genpdf::Document::new(family);
    doc.set_title(format!("Baugeschichte – {}", name));
    doc.set_minimal_conformance();
    let mut deco = genpdf::SimplePageDecorator::new();
    deco.set_margins(18);
    doc.set_page_decorator(deco);

    // Erica's own messages, chronological.
    let mut hers: Vec<&Value> = messages
        .iter()
        .filter(|m| !m.get("fromMe").and_then(Value::as_bool).unwrap_or(false))
        .collect();
    hers.sort_by_key(|m| m.get("ts").and_then(Value::as_i64).unwrap_or(0));

    let file_of = |m: &Value| m.get("file").and_then(Value::as_str).map(str::to_string);
    let text_of = |m: &Value| m.get("text").and_then(Value::as_str).unwrap_or("").to_string();

    let photos: Vec<&&Value> = hers
        .iter()
        .filter(|m| file_of(m).map(|f| is_image(&f)).unwrap_or(false))
        .collect();
    let text_notes: Vec<&&Value> = hers
        .iter()
        .filter(|m| file_of(m).is_none() && !text_of(m).trim().is_empty())
        .collect();

    // --- Cover ---
    // Generous vertical breaks between the title lines: pushing two large
    // paragraphs back-to-back makes the title look squashed.
    doc.push(Break::new(6.0));
    push_lines(
        &mut doc,
        lang.kicker(),
        Style::new().with_color(GOLD).with_font_size(11).bold(),
        Alignment::Center,
    );
    doc.push(Break::new(2.5));
    push_lines(
        &mut doc,
        lang.house_of(),
        Style::new().with_color(BROWN).with_font_size(19),
        Alignment::Center,
    );
    doc.push(Break::new(1.6));
    push_lines(
        &mut doc,
        name,
        Style::new().with_color(INK).with_font_size(32).bold(),
        Alignment::Center,
    );
    doc.push(Break::new(2.0));
    push_lines(
        &mut doc,
        lang.place(),
        Style::new().with_color(BROWN).with_font_size(14).italic(),
        Alignment::Center,
    );
    doc.push(Break::new(2.5));
    push_lines(
        &mut doc,
        lang.lead(),
        Style::new().with_color(BROWN).with_font_size(11),
        Alignment::Center,
    );
    doc.push(Break::new(2.0));
    push_lines(
        &mut doc,
        &format!("{}: {}", lang.location_label(), MAPS_URL),
        Style::new().with_color(BROWN).with_font_size(COVER_LINK_FONT_SIZE),
        Alignment::Center,
    );
    doc.push(Break::new(0.4));
    push_lines(
        &mut doc,
        &format!("{}: {}", lang.listing_label(), LISTING_URL),
        Style::new().with_color(BROWN).with_font_size(COVER_LINK_FONT_SIZE),
        Alignment::Center,
    );
    doc.push(PageBreak::new());

    // --- Intro ---
    if let Some(first) = text_notes.first() {
        let intro = translate(lang, &text_of(first));
        push_lines(
            &mut doc,
            lang.intro_heading(),
            Style::new().with_color(BROWN).with_font_size(15).bold(),
            Alignment::Left,
        );
        doc.push(Break::new(0.5));
        push_lines(
            &mut doc,
            &intro,
            Style::new().with_color(INK).with_font_size(11).italic(),
            Alignment::Left,
        );
        doc.push(Break::new(1.5));
    }

    // --- Photo plates ---
    // genpdf can't split or keep-together an image, and a tall portrait photo
    // starting mid-page would overflow the bottom edge. So give each plate its
    // own page: label + image + caption then always fit.
    for (i, p) in photos.iter().enumerate() {
        let file = file_of(p).unwrap();
        let path = Path::new(DIR).join(&file);

        doc.push(PageBreak::new());
        push_lines(
            &mut doc,
            &format!("{} {}", lang.plate_word(), i + 1),
            Style::new().with_color(GOLD).with_font_size(9),
            Alignment::Center,
        );
        doc.push(Break::new(0.3));

        let (jpeg, fw, fh) = scaled_jpeg(&path)?;
        let img = Image::from_reader(Cursor::new(jpeg))
            .map_err(|e| anyhow!("load image {}: {}", path.display(), e))?
            .with_alignment(Alignment::Center)
            .with_dpi(dpi_for(fw, fh));
        doc.push(img);

        let cap = translate(lang, text_of(p).trim());
        if !cap.is_empty() {
            doc.push(Break::new(0.3));
            push_lines(
                &mut doc,
                &cap,
                Style::new().with_color(INK).with_font_size(11),
                Alignment::Center,
            );
        }
        doc.push(Break::new(1.5));
    }

    // --- Closing note ---
    let closing: Vec<String> = text_notes
        .iter()
        .skip(1)
        .map(|m| translate(lang, text_of(m).trim()))
        .collect();
    if !closing.is_empty() {
        doc.push(PageBreak::new());
        for note in &closing {
            push_lines(
                &mut doc,
                note,
                Style::new().with_color(Color::Rgb(0x4a, 0x40, 0x35)).with_font_size(11),
                Alignment::Left,
            );
            doc.push(Break::new(0.4));
        }
        push_lines(
            &mut doc,
            &format!("— {}", name),
            Style::new().with_color(BROWN).with_font_size(11).italic(),
            Alignment::Right,
        );
    }

    // --- Footer line ---
    doc.push(Break::new(1.5));
    push_lines(
        &mut doc,
        &lang.footer(name),
        Style::new().with_color(Color::Rgb(0x9a, 0x8e, 0x7d)).with_font_size(8),
        Alignment::Center,
    );

    let pdf_path = PathBuf::from(DIR).join(format!("baugeschichte{}.pdf", lang.slug_suffix()));
    doc.render_to_file(&pdf_path)
        .map_err(|e| anyhow!("render {}: {}", pdf_path.display(), e))?;
    // genpdf has no hyperlink support, so overlay clickable link annotations
    // onto the two cover URL lines as a post-process step.
    let added = add_cover_links(&pdf_path)?;
    eprintln!("wrote {} ({} clickable cover links)", pdf_path.display(), added);
    Ok(pdf_path)
}

/// Overlay clickable Link annotations onto the cover's two URL lines. genpdf
/// 0.2 can't emit hyperlinks, so we reopen the finished PDF with lopdf, locate
/// the only two text lines drawn at COVER_LINK_FONT_SIZE on page 1 (reading
/// their glyph-origin from the content stream — the text itself is encoded as
/// CIDs, but the position operators are plain numbers), and attach a /Link with
/// a URI action. Because the lines are centred, the box spans x … (width − x).
/// Returns how many links were added.
fn add_cover_links(pdf_path: &Path) -> Result<usize> {
    use lopdf::{Dictionary, Document, Object, StringFormat};

    let mut doc = Document::load(pdf_path)?;
    let page_id = *doc
        .get_pages()
        .get(&1)
        .ok_or_else(|| anyhow!("PDF has no page 1"))?;
    let content = doc.get_and_decode_page_content(page_id)?;

    let num = |o: &Object| -> Option<f64> {
        match o {
            Object::Real(r) => Some(*r as f64),
            Object::Integer(i) => Some(*i as f64),
            _ => None,
        }
    };

    // Walk the content stream tracking the current text origin + font size;
    // record the origin of each text-show run drawn at the link font size.
    let mut pos = (0.0f64, 0.0f64);
    let mut size = 0.0f64;
    let mut origins: Vec<(f64, f64)> = Vec::new();
    for op in &content.operations {
        match op.operator.as_str() {
            "Td" | "TD" if op.operands.len() >= 2 => {
                if let (Some(x), Some(y)) = (num(&op.operands[0]), num(&op.operands[1])) {
                    pos = (x, y);
                }
            }
            "Tm" if op.operands.len() >= 6 => {
                if let (Some(x), Some(y)) = (num(&op.operands[4]), num(&op.operands[5])) {
                    pos = (x, y);
                }
            }
            "Tf" if op.operands.len() >= 2 => {
                if let Some(s) = num(&op.operands[1]) {
                    size = s;
                }
            }
            "Tj" | "TJ" => {
                if (size - COVER_LINK_FONT_SIZE as f64).abs() < 0.01
                    && origins.last() != Some(&pos)
                {
                    origins.push(pos);
                }
            }
            _ => {}
        }
    }

    // Top-most line first: location (Ort), then the listing (Inserat) — matching
    // the order they are pushed onto the cover.
    origins.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let urls = [MAPS_URL, LISTING_URL];

    let mut annot_refs: Vec<Object> = Vec::new();
    for ((x, y), url) in origins.iter().zip(urls.iter()) {
        let mut action = Dictionary::new();
        action.set("S", Object::Name(b"URI".to_vec()));
        action.set("URI", Object::String(url.as_bytes().to_vec(), StringFormat::Literal));
        let mut annot = Dictionary::new();
        annot.set("Type", Object::Name(b"Annot".to_vec()));
        annot.set("Subtype", Object::Name(b"Link".to_vec()));
        annot.set(
            "Rect",
            Object::Array(vec![
                Object::Real((*x - 2.0) as f32),
                Object::Real((*y - 2.0) as f32),
                Object::Real((A4_WIDTH_PT - *x + 2.0) as f32),
                Object::Real((*y + COVER_LINK_FONT_SIZE as f64 + 2.0) as f32),
            ]),
        );
        annot.set("Border", Object::Array(vec![0.into(), 0.into(), 0.into()]));
        annot.set("A", Object::Dictionary(action));
        let id = doc.add_object(annot);
        annot_refs.push(Object::Reference(id));
    }

    let added = annot_refs.len();
    if added > 0 {
        let page = doc.get_object_mut(page_id)?.as_dict_mut()?;
        match page.get_mut(b"Annots") {
            Ok(Object::Array(arr)) => arr.extend(annot_refs),
            _ => page.set("Annots", Object::Array(annot_refs)),
        }
        doc.save(pdf_path)?;
    }
    Ok(added)
}

fn main() -> Result<()> {
    let mut langs = vec![Lang::De, Lang::El];
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--lang" => {
                let v = args.next().ok_or_else(|| anyhow!("--lang needs a value"))?;
                langs = match v.as_str() {
                    "de" => vec![Lang::De],
                    "el" | "gr" => vec![Lang::El],
                    "both" => vec![Lang::De, Lang::El],
                    other => anyhow::bail!("unknown --lang {} (use de|el|both)", other),
                };
            }
            "-h" | "--help" => {
                println!("baugeschichte [--lang de|el|both]");
                return Ok(());
            }
            other => anyhow::bail!("unknown arg {}", other),
        }
    }

    let font_dir = env::var("FONT_DIR").unwrap_or_else(|_| DEFAULT_FONT_DIR.into());
    let raw = std::fs::read_to_string(PathBuf::from(DIR).join("messages.json"))?;
    let data: Value = serde_json::from_str(&raw)?;
    let m = data
        .get("matches")
        .and_then(|v| v.get(0))
        .ok_or_else(|| anyhow!("messages.json has no matches[0]"))?;
    let name = m.get("name").and_then(Value::as_str).unwrap_or("Erica Baumann");
    let messages = m
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("matches[0].messages is not an array"))?;

    for lang in langs {
        render(lang, name, messages, &font_dir)?;
    }
    Ok(())
}
