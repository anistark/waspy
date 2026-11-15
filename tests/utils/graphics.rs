//! The harness's graphics: typed arrays, a canvas's 2D context, and
//! `ImageData`, as much of each as the tests need.
//!
//! They are host objects (`Js::Host`): Rust values answering property
//! reads, writes, method calls, and items, so a new kind of object needs no
//! change to the value model. The 2D context keeps a real bitmap, but it
//! rasterizes only what a test can predict exactly: an axis-aligned
//! rectangle in an opaque color on whole pixels (`fillRect`, `clearRect`)
//! and `putImageData`. Reading pixels back after anything else it was asked
//! to draw (a stroke, a path, text, a fractional or rotated rectangle, a
//! translucent color) panics, so a test can never assert a guess; those are
//! checked in Chromium by the browser gate.

use super::{js_error, Host, Js, Node};
use std::any::Any;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// A JavaScript object the harness implements in Rust.
pub trait HostObject {
    /// The class `String(obj)` names: `[object <class>]`.
    fn class(&self) -> &'static str;
    fn get(&self, name: &str) -> Option<Js>;
    fn set(&self, name: &str, _value: &Js) -> Result<(), Js> {
        Err(js_error(
            "TypeError",
            format!("Cannot set property {name} of [object {}]", self.class()),
        ))
    }
    /// Whether the object has a method of this name.
    fn has_method(&self, name: &str) -> bool;
    /// A method call, or None when the object has no such method.
    fn call(&self, this: &Js, host: &mut Host, name: &str, args: &[Js]) -> Option<Result<Js, Js>>;
    /// The length of an object Python can index (an array-like one).
    fn length(&self) -> Option<usize> {
        None
    }
    fn item(&self, _i: usize) -> Js {
        Js::Undefined
    }
    fn set_item(&self, _i: usize, _value: &Js) {}
    fn text(&self) -> String {
        format!("[object {}]", self.class())
    }
    fn as_any(&self) -> &dyn Any;
}

fn arg(args: &[Js], k: usize) -> Js {
    args.get(k).cloned().unwrap_or(Js::Undefined)
}

fn num(args: &[Js], k: usize) -> f64 {
    arg(args, k).number()
}

/// The host object a value is, as `T`.
pub fn host<T: 'static>(value: &Js) -> Option<&T> {
    match value {
        Js::Host(o) => o.as_any().downcast_ref::<T>(),
        _ => None,
    }
}

// ---- typed arrays ---------------------------------------------------------

/// A typed array's element type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    U8,
    U8Clamped,
    U16,
    I32,
    F32,
    F64,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::U8,
        Kind::U8Clamped,
        Kind::U16,
        Kind::I32,
        Kind::F32,
        Kind::F64,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Kind::U8 => "Uint8Array",
            Kind::U8Clamped => "Uint8ClampedArray",
            Kind::U16 => "Uint16Array",
            Kind::I32 => "Int32Array",
            Kind::F32 => "Float32Array",
            Kind::F64 => "Float64Array",
        }
    }
    pub fn bytes(self) -> usize {
        match self {
            Kind::U8 | Kind::U8Clamped => 1,
            Kind::U16 => 2,
            Kind::I32 | Kind::F32 => 4,
            Kind::F64 => 8,
        }
    }
    /// A number stored into an element, as JavaScript converts it.
    pub fn coerce(self, x: f64) -> f64 {
        let modular = |m: f64| {
            if !x.is_finite() {
                return 0.0;
            }
            x.trunc().rem_euclid(m)
        };
        match self {
            Kind::U8Clamped => {
                if x.is_nan() {
                    0.0
                } else {
                    // Rounded half to even, as ToUint8Clamp does.
                    let c = x.clamp(0.0, 255.0);
                    let f = c.floor();
                    if c - f > 0.5 || (c - f == 0.5 && f % 2.0 != 0.0) {
                        f + 1.0
                    } else {
                        f
                    }
                }
            }
            Kind::U8 => modular(256.0),
            Kind::U16 => modular(65536.0),
            Kind::I32 => {
                let u = modular(4294967296.0);
                if u >= 2147483648.0 {
                    u - 4294967296.0
                } else {
                    u
                }
            }
            Kind::F32 => x as f32 as f64,
            Kind::F64 => x,
        }
    }
}

pub struct TypedArray {
    pub kind: Kind,
    pub data: RefCell<Vec<f64>>,
}

impl TypedArray {
    pub fn make(kind: Kind, values: impl IntoIterator<Item = f64>) -> Js {
        Js::Host(Rc::new(TypedArray {
            kind,
            data: RefCell::new(values.into_iter().map(|x| kind.coerce(x)).collect()),
        }))
    }
    /// The array's elements as bytes, little-endian, as its buffer holds them.
    pub fn bytes(&self) -> Vec<u8> {
        let data = self.data.borrow();
        let mut out = Vec::with_capacity(data.len() * self.kind.bytes());
        for &x in data.iter() {
            match self.kind {
                Kind::U8 | Kind::U8Clamped => out.push(x as u8),
                Kind::U16 => out.extend((x as u16).to_le_bytes()),
                Kind::I32 => out.extend((x as i32).to_le_bytes()),
                Kind::F32 => out.extend((x as f32).to_le_bytes()),
                Kind::F64 => out.extend(x.to_le_bytes()),
            }
        }
        out
    }
    /// The values a constructor's or `set()`'s argument holds.
    fn values_of(source: &Js) -> Result<Vec<f64>, Js> {
        match source {
            Js::Arr(items) => Ok(items.borrow().iter().map(Js::number).collect()),
            other => match host::<TypedArray>(other) {
                Some(t) => Ok(t.data.borrow().clone()),
                None => Err(js_error("TypeError", "not an array or a typed array")),
            },
        }
    }
    /// `new Uint8Array(n)`, `new Uint8Array(array)`, `new Uint8Array(typed)`.
    pub fn construct(kind: Kind, args: &[Js]) -> Result<Js, Js> {
        match arg(args, 0) {
            Js::Undefined => Ok(TypedArray::make(kind, [])),
            Js::Num(n) if n >= 0.0 && n == n.trunc() => {
                Ok(TypedArray::make(kind, vec![0.0; n as usize]))
            }
            Js::Num(_) => Err(js_error("RangeError", "Invalid typed array length")),
            source => Ok(TypedArray::make(kind, TypedArray::values_of(&source)?)),
        }
    }
}

impl HostObject for TypedArray {
    fn class(&self) -> &'static str {
        self.kind.name()
    }
    fn get(&self, name: &str) -> Option<Js> {
        let n = self.data.borrow().len();
        Some(match name {
            "length" => Js::Num(n as f64),
            "byteLength" => Js::Num((n * self.kind.bytes()) as f64),
            "BYTES_PER_ELEMENT" => Js::Num(self.kind.bytes() as f64),
            _ => return None,
        })
    }
    fn has_method(&self, name: &str) -> bool {
        matches!(name, "fill" | "set")
    }
    fn call(&self, this: &Js, _: &mut Host, name: &str, args: &[Js]) -> Option<Result<Js, Js>> {
        Some(match name {
            "fill" => {
                let x = self.kind.coerce(num(args, 0));
                self.data.borrow_mut().iter_mut().for_each(|v| *v = x);
                Ok(this.clone())
            }
            "set" => (|| {
                let values = TypedArray::values_of(&arg(args, 0))?;
                let offset = match arg(args, 1) {
                    Js::Undefined => 0,
                    o => o.number() as usize,
                };
                let mut data = self.data.borrow_mut();
                if offset + values.len() > data.len() {
                    return Err(js_error("RangeError", "offset is out of bounds"));
                }
                for (k, x) in values.into_iter().enumerate() {
                    data[offset + k] = self.kind.coerce(x);
                }
                Ok(Js::Undefined)
            })(),
            _ => return None,
        })
    }
    fn length(&self) -> Option<usize> {
        Some(self.data.borrow().len())
    }
    fn item(&self, i: usize) -> Js {
        Js::Num(self.data.borrow()[i])
    }
    fn set_item(&self, i: usize, value: &Js) {
        self.data.borrow_mut()[i] = self.kind.coerce(value.number());
    }
    fn text(&self) -> String {
        self.data
            .borrow()
            .iter()
            .map(|x| Js::Num(*x).text())
            .collect::<Vec<_>>()
            .join(",")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- ImageData ------------------------------------------------------------

pub struct ImageData {
    pub width: usize,
    pub height: usize,
    /// A `Uint8ClampedArray` of `width * height * 4` bytes, RGBA.
    pub data: Js,
}

impl ImageData {
    pub fn make(width: usize, height: usize, pixels: Vec<u8>) -> Js {
        Js::Host(Rc::new(ImageData {
            width,
            height,
            data: TypedArray::make(Kind::U8Clamped, pixels.into_iter().map(f64::from)),
        }))
    }
    pub fn pixels(&self) -> Vec<u8> {
        host::<TypedArray>(&self.data)
            .expect("a Uint8ClampedArray")
            .bytes()
    }
    /// `new ImageData(width, height)` or `new ImageData(data, width[, height])`.
    pub fn construct(args: &[Js]) -> Result<Js, Js> {
        match arg(args, 0) {
            Js::Num(w) => {
                let h = num(args, 1);
                Ok(ImageData::make(
                    w as usize,
                    h as usize,
                    vec![0; (w * h * 4.0) as usize],
                ))
            }
            data => {
                let Some(t) = host::<TypedArray>(&data).filter(|t| t.kind == Kind::U8Clamped)
                else {
                    return Err(js_error(
                        "TypeError",
                        "Failed to construct 'ImageData': parameter 1 is not of type 'Uint8ClampedArray'",
                    ));
                };
                let w = num(args, 1) as usize;
                let n = t.data.borrow().len();
                if w == 0 || n % (4 * w) != 0 {
                    return Err(js_error(
                        "IndexSizeError",
                        "Failed to construct 'ImageData': The input data length is not a multiple of (4 * width).",
                    ));
                }
                Ok(Js::Host(Rc::new(ImageData {
                    width: w,
                    height: n / (4 * w),
                    data,
                })))
            }
        }
    }
}

impl HostObject for ImageData {
    fn class(&self) -> &'static str {
        "ImageData"
    }
    fn get(&self, name: &str) -> Option<Js> {
        Some(match name {
            "width" => Js::Num(self.width as f64),
            "height" => Js::Num(self.height as f64),
            "data" => self.data.clone(),
            "colorSpace" => Js::Str("srgb".to_string()),
            _ => return None,
        })
    }
    fn has_method(&self, _: &str) -> bool {
        false
    }
    fn call(&self, _: &Js, _: &mut Host, _: &str, _: &[Js]) -> Option<Result<Js, Js>> {
        None
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- colors ---------------------------------------------------------------

/// A CSS color: red, green, blue, and its alpha as written.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Color(pub [u8; 3], pub f64);

impl Color {
    fn parse(text: &str) -> Option<Color> {
        let s = text.trim().to_ascii_lowercase();
        if let Some(hex) = s.strip_prefix('#') {
            let digit = |k: usize| u8::from_str_radix(&hex[k..k + 1], 16).ok();
            let pair = |k: usize| u8::from_str_radix(&hex[k..k + 2], 16).ok();
            return match hex.len() {
                3 => Some(Color([digit(0)? * 17, digit(1)? * 17, digit(2)? * 17], 1.0)),
                4 => Some(Color(
                    [digit(0)? * 17, digit(1)? * 17, digit(2)? * 17],
                    f64::from(digit(3)? * 17) / 255.0,
                )),
                6 => Some(Color([pair(0)?, pair(2)?, pair(4)?], 1.0)),
                8 => Some(Color(
                    [pair(0)?, pair(2)?, pair(4)?],
                    f64::from(pair(6)?) / 255.0,
                )),
                _ => None,
            };
        }
        let inner = s
            .strip_prefix("rgba(")
            .or_else(|| s.strip_prefix("rgb("))
            .and_then(|r| r.strip_suffix(')'));
        if let Some(inner) = inner {
            let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            if parts.len() != 3 && parts.len() != 4 {
                return None;
            }
            let channel = |p: &str| -> Option<u8> {
                let x: f64 = p.parse().ok()?;
                Some(x.clamp(0.0, 255.0).round() as u8)
            };
            let alpha = match parts.get(3) {
                Some(p) => p.parse::<f64>().ok()?.clamp(0.0, 1.0),
                None => 1.0,
            };
            return Some(Color(
                [channel(parts[0])?, channel(parts[1])?, channel(parts[2])?],
                alpha,
            ));
        }
        let rgb = match s.as_str() {
            "transparent" => return Some(Color([0, 0, 0], 0.0)),
            "black" => [0, 0, 0],
            "white" => [255, 255, 255],
            "red" => [255, 0, 0],
            "green" => [0, 128, 0],
            "lime" => [0, 255, 0],
            "blue" => [0, 0, 255],
            "yellow" => [255, 255, 0],
            "orange" => [255, 165, 0],
            "purple" => [128, 0, 128],
            "gray" | "grey" => [128, 128, 128],
            "silver" => [192, 192, 192],
            "navy" => [0, 0, 128],
            "teal" => [0, 128, 128],
            "maroon" => [128, 0, 0],
            "olive" => [128, 128, 0],
            "aqua" | "cyan" => [0, 255, 255],
            "fuchsia" | "magenta" => [255, 0, 255],
            // A word could be a CSS color the harness lacks; anything else
            // is no color, which the context ignores.
            other if other.chars().all(|c| c.is_ascii_alphabetic()) => {
                panic!("the harness canvas does not know the color {other:?}")
            }
            _ => return None,
        };
        Some(Color(rgb, 1.0))
    }
    /// The color as a 2D context answers it: `#rrggbb`, or `rgba(...)` when
    /// it is not opaque.
    fn text(self) -> String {
        let [r, g, b] = self.0;
        if self.1 == 1.0 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("rgba({r}, {g}, {b}, {})", Js::Num(self.1).text())
        }
    }
}

// ---- the 2D context -------------------------------------------------------

/// A fill or stroke style: a color, or another object (a gradient, a
/// pattern), which the harness does not paint with.
#[derive(Clone)]
enum Style {
    Color(Color),
    Other(Js),
}

impl Style {
    fn value(&self) -> Js {
        match self {
            Style::Color(c) => Js::Str(c.text()),
            Style::Other(v) => v.clone(),
        }
    }
    fn set(&mut self, value: &Js) {
        match value {
            // An unparsable color is ignored, as a browser ignores it.
            Js::Str(s) => {
                if let Some(c) = Color::parse(s) {
                    *self = Style::Color(c);
                }
            }
            Js::Host(_) | Js::Obj(..) => *self = Style::Other(value.clone()),
            _ => {}
        }
    }
}

/// What `save()` keeps and `restore()` brings back.
#[derive(Clone)]
struct Paint {
    fill: Style,
    stroke: Style,
    line_width: f64,
    alpha: f64,
    font: String,
    /// The transform `[a, b, c, d, e, f]`.
    matrix: [f64; 6],
}

impl Default for Paint {
    fn default() -> Paint {
        Paint {
            fill: Style::Color(Color([0, 0, 0], 1.0)),
            stroke: Style::Color(Color([0, 0, 0], 1.0)),
            line_width: 1.0,
            alpha: 1.0,
            font: "10px sans-serif".to_string(),
            matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        }
    }
}

struct Ctx2DState {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
    paint: Paint,
    stack: Vec<Paint>,
    /// The first drawing operation the harness did not rasterize.
    unmodeled: Option<String>,
}

pub struct Ctx2D {
    canvas: Weak<RefCell<Node>>,
    state: RefCell<Ctx2DState>,
}

/// The size a canvas element declares (`width`, `height`).
fn canvas_size(node: &Node) -> (usize, usize) {
    let prop = |name: &str, default: f64| {
        node.props
            .iter()
            .find(|(k, _)| k == name)
            .map_or(default, |(_, v)| v.number())
    };
    (
        prop("width", 300.0) as usize,
        prop("height", 150.0) as usize,
    )
}

/// The property a canvas keeps its 2D context in.
const CONTEXT_PROP: &str = "__harness_context_2d";

impl Ctx2D {
    /// A canvas's 2D context, made on first use (`getContext("2d")`).
    pub fn of(canvas: &Rc<RefCell<Node>>) -> Js {
        if let Some((_, ctx)) = canvas
            .borrow()
            .props
            .iter()
            .find(|(k, _)| k == CONTEXT_PROP)
        {
            return ctx.clone();
        }
        let (width, height) = canvas_size(&canvas.borrow());
        let ctx = Js::Host(Rc::new(Ctx2D {
            canvas: Rc::downgrade(canvas),
            state: RefCell::new(Ctx2DState {
                width,
                height,
                pixels: vec![0; width * height * 4],
                paint: Paint::default(),
                stack: Vec::new(),
                unmodeled: None,
            }),
        }));
        canvas
            .borrow_mut()
            .props
            .push((CONTEXT_PROP.to_string(), ctx.clone()));
        ctx
    }

    /// The canvas was given a width or height: its bitmap is cleared to the
    /// new size and its context's state reset, as a browser does.
    pub fn resized(canvas: &Rc<RefCell<Node>>) {
        let ctx = canvas
            .borrow()
            .props
            .iter()
            .find(|(k, _)| k == CONTEXT_PROP)
            .map(|(_, v)| v.clone());
        if let Some(ctx) = ctx {
            let (width, height) = canvas_size(&canvas.borrow());
            let ctx = host::<Ctx2D>(&ctx).expect("a 2D context");
            let mut s = ctx.state.borrow_mut();
            s.width = width;
            s.height = height;
            s.pixels = vec![0; width * height * 4];
            s.paint = Paint::default();
            s.stack.clear();
            s.unmodeled = None;
        }
    }

    /// Fill the rectangle with an opaque color, or clear it (`None`).
    fn rect(&self, args: &[Js], color: Option<[u8; 4]>) {
        let mut s = self.state.borrow_mut();
        let [a, b, c, d, e, f] = s.paint.matrix;
        assert!(
            b == 0.0 && c == 0.0,
            "the harness canvas draws axis-aligned rectangles only"
        );
        let (x, y, w, h) = (num(args, 0), num(args, 1), num(args, 2), num(args, 3));
        let (x0, x1) = (a * x + e, a * (x + w) + e);
        let (y0, y1) = (d * y + f, d * (y + h) + f);
        let (x0, x1) = (x0.min(x1), x0.max(x1));
        let (y0, y1) = (y0.min(y1), y0.max(y1));
        assert!(
            [x0, x1, y0, y1].iter().all(|v| *v == v.trunc()),
            "the harness canvas draws on whole pixels only ({x0}, {y0}, {x1}, {y1})"
        );
        let clamp = |v: f64, max: usize| v.clamp(0.0, max as f64) as usize;
        let (x0, x1) = (clamp(x0, s.width), clamp(x1, s.width));
        let (y0, y1) = (clamp(y0, s.height), clamp(y1, s.height));
        let rgba = color.unwrap_or([0; 4]);
        let width = s.width;
        for py in y0..y1 {
            for px in x0..x1 {
                let at = (py * width + px) * 4;
                s.pixels[at..at + 4].copy_from_slice(&rgba);
            }
        }
    }

    fn fill_rect(&self, args: &[Js]) {
        let (style, alpha) = {
            let s = self.state.borrow();
            (s.paint.fill.clone(), s.paint.alpha)
        };
        let Style::Color(Color([r, g, b], a)) = style else {
            self.unmodeled("fillRect with a gradient or pattern");
            return;
        };
        if a != 1.0 || alpha != 1.0 {
            self.unmodeled("fillRect in a translucent color");
            return;
        }
        self.rect(args, Some([r, g, b, 255]));
    }

    fn unmodeled(&self, what: &str) {
        let mut s = self.state.borrow_mut();
        if s.unmodeled.is_none() {
            s.unmodeled = Some(what.to_string());
        }
    }

    fn image_data(&self, args: &[Js]) -> Js {
        let s = self.state.borrow();
        if let Some(what) = &s.unmodeled {
            panic!(
                "the harness canvas did not rasterize {what}, so it cannot read pixels back \
                 (the browser gate checks those in Chromium)"
            );
        }
        let (x, y) = (num(args, 0) as i64, num(args, 1) as i64);
        let (w, h) = (num(args, 2) as usize, num(args, 3) as usize);
        let mut out = vec![0; w * h * 4];
        for row in 0..h {
            for col in 0..w {
                let (px, py) = (x + col as i64, y + row as i64);
                if px < 0 || py < 0 || px as usize >= s.width || py as usize >= s.height {
                    continue;
                }
                let from = (py as usize * s.width + px as usize) * 4;
                let to = (row * w + col) * 4;
                out[to..to + 4].copy_from_slice(&s.pixels[from..from + 4]);
            }
        }
        ImageData::make(w, h, out)
    }

    fn put_image_data(&self, args: &[Js]) -> Result<Js, Js> {
        let image = arg(args, 0);
        let Some(img) = host::<ImageData>(&image) else {
            return Err(js_error(
                "TypeError",
                "Failed to execute 'putImageData' on 'CanvasRenderingContext2D': parameter 1 is not of type 'ImageData'.",
            ));
        };
        let pixels = img.pixels();
        let (dx, dy) = (num(args, 1) as i64, num(args, 2) as i64);
        let mut s = self.state.borrow_mut();
        let width = s.width;
        for row in 0..img.height {
            for col in 0..img.width {
                let (px, py) = (dx + col as i64, dy + row as i64);
                if px < 0 || py < 0 || px as usize >= s.width || py as usize >= s.height {
                    continue;
                }
                let to = (py as usize * width + px as usize) * 4;
                let from = (row * img.width + col) * 4;
                s.pixels[to..to + 4].copy_from_slice(&pixels[from..from + 4]);
            }
        }
        Ok(Js::Undefined)
    }

    fn transform(&self, m: [f64; 6]) {
        let mut s = self.state.borrow_mut();
        let [a, b, c, d, e, f] = s.paint.matrix;
        let [a2, b2, c2, d2, e2, f2] = m;
        s.paint.matrix = [
            a * a2 + c * b2,
            b * a2 + d * b2,
            a * c2 + c * d2,
            b * c2 + d * d2,
            a * e2 + c * f2 + e,
            b * e2 + d * f2 + f,
        ];
    }
}

/// The path methods: they build a path the harness does not keep.
const PATH_METHODS: [&str; 10] = [
    "beginPath",
    "closePath",
    "moveTo",
    "lineTo",
    "arc",
    "arcTo",
    "rect",
    "ellipse",
    "quadraticCurveTo",
    "bezierCurveTo",
];

/// The context's other methods.
const CTX_METHODS: [&str; 13] = [
    "fillRect",
    "clearRect",
    "getImageData",
    "putImageData",
    "createImageData",
    "save",
    "restore",
    "translate",
    "scale",
    "rotate",
    "setTransform",
    "resetTransform",
    "getTransform",
];

/// The methods that draw what the harness does not rasterize.
const UNMODELED_DRAWING: [&str; 6] = [
    "fill",
    "stroke",
    "strokeRect",
    "fillText",
    "strokeText",
    "drawImage",
];

impl HostObject for Ctx2D {
    fn class(&self) -> &'static str {
        "CanvasRenderingContext2D"
    }
    fn get(&self, name: &str) -> Option<Js> {
        let s = self.state.borrow();
        Some(match name {
            "canvas" => Js::Node(self.canvas.upgrade().expect("the canvas")),
            "fillStyle" => s.paint.fill.value(),
            "strokeStyle" => s.paint.stroke.value(),
            "lineWidth" => Js::Num(s.paint.line_width),
            "globalAlpha" => Js::Num(s.paint.alpha),
            "font" => Js::Str(s.paint.font.clone()),
            _ => return None,
        })
    }
    fn set(&self, name: &str, value: &Js) -> Result<(), Js> {
        let mut s = self.state.borrow_mut();
        match name {
            "fillStyle" => s.paint.fill.set(value),
            "strokeStyle" => s.paint.stroke.set(value),
            // A width or alpha out of range is ignored, as a browser does.
            "lineWidth" => {
                let w = value.number();
                if w.is_finite() && w > 0.0 {
                    s.paint.line_width = w;
                }
            }
            "globalAlpha" => {
                let a = value.number();
                if (0.0..=1.0).contains(&a) {
                    s.paint.alpha = a;
                }
            }
            "font" => s.paint.font = value.text(),
            other => panic!("the harness 2D context has no property {other:?}"),
        }
        Ok(())
    }
    fn has_method(&self, name: &str) -> bool {
        CTX_METHODS.contains(&name)
            || PATH_METHODS.contains(&name)
            || UNMODELED_DRAWING.contains(&name)
    }
    fn call(&self, _: &Js, _: &mut Host, name: &str, args: &[Js]) -> Option<Result<Js, Js>> {
        Some(Ok(match name {
            "fillRect" => {
                self.fill_rect(args);
                Js::Undefined
            }
            "clearRect" => {
                self.rect(args, None);
                Js::Undefined
            }
            "getImageData" => self.image_data(args),
            "putImageData" => return Some(self.put_image_data(args)),
            "createImageData" => match host::<ImageData>(&arg(args, 0)) {
                Some(img) => {
                    ImageData::make(img.width, img.height, vec![0; img.width * img.height * 4])
                }
                None => {
                    let (w, h) = (num(args, 0) as usize, num(args, 1) as usize);
                    ImageData::make(w, h, vec![0; w * h * 4])
                }
            },
            "save" => {
                let mut s = self.state.borrow_mut();
                let paint = s.paint.clone();
                s.stack.push(paint);
                Js::Undefined
            }
            "restore" => {
                let mut s = self.state.borrow_mut();
                if let Some(p) = s.stack.pop() {
                    s.paint = p;
                }
                Js::Undefined
            }
            "translate" => {
                self.transform([1.0, 0.0, 0.0, 1.0, num(args, 0), num(args, 1)]);
                Js::Undefined
            }
            "scale" => {
                self.transform([num(args, 0), 0.0, 0.0, num(args, 1), 0.0, 0.0]);
                Js::Undefined
            }
            "rotate" => {
                let (sin, cos) = num(args, 0).sin_cos();
                self.transform([cos, sin, -sin, cos, 0.0, 0.0]);
                Js::Undefined
            }
            "setTransform" => {
                self.state.borrow_mut().paint.matrix = [0, 1, 2, 3, 4, 5].map(|k| num(args, k));
                Js::Undefined
            }
            "resetTransform" => {
                self.state.borrow_mut().paint.matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                Js::Undefined
            }
            "getTransform" => {
                let m = self.state.borrow().paint.matrix;
                Js::object(
                    ["a", "b", "c", "d", "e", "f"]
                        .into_iter()
                        .zip(m)
                        .map(|(k, v)| (k, Js::Num(v)))
                        .collect(),
                )
            }
            p if PATH_METHODS.contains(&p) => Js::Undefined,
            d if UNMODELED_DRAWING.contains(&d) => {
                self.unmodeled(d);
                Js::Undefined
            }
            _ => return None,
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- WebGL -----------------------------------------------------------------

/// The WebGL constants the tests use, as the specification numbers them.
const GL_CONSTANTS: [(&str, f64); 26] = [
    ("DEPTH_BUFFER_BIT", 256.0),
    ("COLOR_BUFFER_BIT", 16384.0),
    ("POINTS", 0.0),
    ("LINES", 1.0),
    ("TRIANGLES", 4.0),
    ("NO_ERROR", 0.0),
    ("INVALID_ENUM", 1280.0),
    ("ARRAY_BUFFER", 34962.0),
    ("ELEMENT_ARRAY_BUFFER", 34963.0),
    ("STATIC_DRAW", 35044.0),
    ("DYNAMIC_DRAW", 35048.0),
    ("BUFFER_SIZE", 34660.0),
    ("FLOAT", 5126.0),
    ("UNSIGNED_BYTE", 5121.0),
    ("UNSIGNED_SHORT", 5123.0),
    ("RGBA", 6408.0),
    ("FRAGMENT_SHADER", 35632.0),
    ("VERTEX_SHADER", 35633.0),
    ("COMPILE_STATUS", 35713.0),
    ("LINK_STATUS", 35714.0),
    ("DEPTH_TEST", 2929.0),
    ("BLEND", 3042.0),
    ("TEXTURE_2D", 3553.0),
    ("TEXTURE0", 33984.0),
    ("CULL_FACE", 2884.0),
    ("VIEWPORT", 2978.0),
];

/// A WebGL object (a shader, a program, a buffer, a uniform's location),
/// which only names itself.
pub struct GlObject {
    class: &'static str,
    id: usize,
}

impl HostObject for GlObject {
    fn class(&self) -> &'static str {
        self.class
    }
    fn get(&self, _: &str) -> Option<Js> {
        None
    }
    fn has_method(&self, _: &str) -> bool {
        false
    }
    fn call(&self, _: &Js, _: &mut Host, _: &str, _: &[Js]) -> Option<Result<Js, Js>> {
        None
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct GlState {
    width: usize,
    height: usize,
    /// The framebuffer, RGBA, its first row the bottom one.
    pixels: Vec<u8>,
    clear: [f64; 4],
    /// Each buffer's size in bytes, by id, and the ids bound to
    /// `ARRAY_BUFFER` and `ELEMENT_ARRAY_BUFFER`.
    buffers: Vec<usize>,
    bound: [Option<usize>; 2],
    objects: usize,
    /// The first drawing call the harness did not rasterize.
    unmodeled: Option<String>,
}

/// A canvas's `webgl` context. It takes every shader as compiling and every
/// program as linking (it reads no GLSL), keeps each buffer's size, and
/// rasterizes only `clear()`: reading pixels back after a draw call panics,
/// since what a GPU draws is checked in Chromium by the browser gate.
pub struct WebGl {
    canvas: Weak<RefCell<Node>>,
    state: RefCell<GlState>,
}

/// The property a canvas keeps its WebGL context in.
const WEBGL_PROP: &str = "__harness_context_webgl";

/// The WebGL methods that change nothing the harness keeps.
const GL_NO_OPS: [&str; 20] = [
    "shaderSource",
    "compileShader",
    "attachShader",
    "linkProgram",
    "useProgram",
    "deleteShader",
    "deleteProgram",
    "deleteBuffer",
    "enableVertexAttribArray",
    "disableVertexAttribArray",
    "vertexAttribPointer",
    "uniform1f",
    "uniform1i",
    "uniform2f",
    "uniform3f",
    "uniform4f",
    "uniformMatrix4fv",
    "viewport",
    "enable",
    "disable",
];

/// The WebGL methods that draw what the harness does not rasterize.
const GL_DRAWING: [&str; 2] = ["drawArrays", "drawElements"];

/// The WebGL methods the harness answers.
const GL_METHODS: [&str; 14] = [
    "createShader",
    "createProgram",
    "createBuffer",
    "getShaderParameter",
    "getProgramParameter",
    "getShaderInfoLog",
    "getProgramInfoLog",
    "bindBuffer",
    "bufferData",
    "getBufferParameter",
    "getAttribLocation",
    "getUniformLocation",
    "clearColor",
    "clear",
];

impl WebGl {
    /// A canvas's WebGL context, made on first use, or null when it already
    /// has a 2D one (and the reverse, in `Ctx2D::of`'s caller).
    pub fn of(canvas: &Rc<RefCell<Node>>) -> Js {
        let has = |key: &str| {
            canvas
                .borrow()
                .props
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        if let Some(gl) = has(WEBGL_PROP) {
            return gl;
        }
        if has(CONTEXT_PROP).is_some() {
            return Js::Null;
        }
        let (width, height) = canvas_size(&canvas.borrow());
        let gl = Js::Host(Rc::new(WebGl {
            canvas: Rc::downgrade(canvas),
            state: RefCell::new(GlState {
                width,
                height,
                pixels: vec![0; width * height * 4],
                clear: [0.0; 4],
                buffers: Vec::new(),
                bound: [None, None],
                objects: 0,
                unmodeled: None,
            }),
        }));
        canvas
            .borrow_mut()
            .props
            .push((WEBGL_PROP.to_string(), gl.clone()));
        gl
    }

    /// Whether a canvas has a WebGL context, so `getContext("2d")` answers
    /// null.
    pub fn on(canvas: &Rc<RefCell<Node>>) -> bool {
        canvas.borrow().props.iter().any(|(k, _)| k == WEBGL_PROP)
    }

    fn object(&self, class: &'static str) -> Js {
        let mut s = self.state.borrow_mut();
        s.objects += 1;
        Js::Host(Rc::new(GlObject {
            class,
            id: s.objects,
        }))
    }

    /// The index of the binding point a target names.
    fn target(args: &[Js]) -> usize {
        match num(args, 0) as i64 {
            34962 => 0,
            34963 => 1,
            other => panic!(
                "the harness WebGL binds ARRAY_BUFFER and ELEMENT_ARRAY_BUFFER only, not {other}"
            ),
        }
    }

    fn read_pixels(&self, args: &[Js]) {
        let s = self.state.borrow();
        if let Some(what) = &s.unmodeled {
            panic!(
                "the harness WebGL did not rasterize {what}, so it cannot read pixels back (the \
                 browser gate checks those in Chromium)"
            );
        }
        let (x, y) = (num(args, 0) as usize, num(args, 1) as usize);
        let (w, h) = (num(args, 2) as usize, num(args, 3) as usize);
        let out = arg(args, 6);
        let t = host::<TypedArray>(&out).expect("readPixels into a typed array");
        for row in 0..h {
            for col in 0..w {
                let from = ((y + row) * s.width + x + col) * 4;
                for k in 0..4 {
                    let value = Js::Num(f64::from(s.pixels[from + k]));
                    t.set_item((row * w + col) * 4 + k, &value);
                }
            }
        }
    }
}

impl HostObject for WebGl {
    fn class(&self) -> &'static str {
        "WebGLRenderingContext"
    }
    fn get(&self, name: &str) -> Option<Js> {
        let s = self.state.borrow();
        match name {
            "canvas" => Some(Js::Node(self.canvas.upgrade().expect("the canvas"))),
            "drawingBufferWidth" => Some(Js::Num(s.width as f64)),
            "drawingBufferHeight" => Some(Js::Num(s.height as f64)),
            _ => GL_CONSTANTS
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| Js::Num(*v)),
        }
    }
    fn has_method(&self, name: &str) -> bool {
        GL_METHODS.contains(&name)
            || GL_NO_OPS.contains(&name)
            || GL_DRAWING.contains(&name)
            || matches!(name, "readPixels" | "getError")
    }
    fn call(&self, _: &Js, _: &mut Host, name: &str, args: &[Js]) -> Option<Result<Js, Js>> {
        Some(Ok(match name {
            "createShader" => self.object("WebGLShader"),
            "createProgram" => self.object("WebGLProgram"),
            "getUniformLocation" => self.object("WebGLUniformLocation"),
            "createBuffer" => {
                let mut s = self.state.borrow_mut();
                s.buffers.push(0);
                let id = s.buffers.len() - 1;
                drop(s);
                Js::Host(Rc::new(GlObject {
                    class: "WebGLBuffer",
                    id,
                }))
            }
            "getShaderParameter" | "getProgramParameter" => Js::Bool(true),
            "getShaderInfoLog" | "getProgramInfoLog" => Js::Str(String::new()),
            "getAttribLocation" => Js::Num(0.0),
            "bindBuffer" => {
                let buffer = host::<GlObject>(&arg(args, 1)).map(|b| b.id);
                self.state.borrow_mut().bound[WebGl::target(args)] = buffer;
                Js::Undefined
            }
            "bufferData" => {
                let size = match arg(args, 1) {
                    Js::Num(n) => n as usize,
                    data => {
                        let t = host::<TypedArray>(&data).expect("bufferData of a typed array");
                        t.data.borrow().len() * t.kind.bytes()
                    }
                };
                let mut s = self.state.borrow_mut();
                let bound = s.bound[WebGl::target(args)].expect("a bound buffer");
                s.buffers[bound] = size;
                Js::Undefined
            }
            "getBufferParameter" => {
                let s = self.state.borrow();
                let bound = s.bound[WebGl::target(args)].expect("a bound buffer");
                Js::Num(s.buffers[bound] as f64)
            }
            "clearColor" => {
                self.state.borrow_mut().clear = [0, 1, 2, 3].map(|k| num(args, k));
                Js::Undefined
            }
            "clear" => {
                if num(args, 0) as i64 & 16384 != 0 {
                    let mut s = self.state.borrow_mut();
                    let rgba = s.clear.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
                    for px in s.pixels.chunks_mut(4) {
                        px.copy_from_slice(&rgba);
                    }
                }
                Js::Undefined
            }
            "readPixels" => {
                self.read_pixels(args);
                Js::Undefined
            }
            "getError" => Js::Num(0.0),
            n if GL_NO_OPS.contains(&n) => Js::Undefined,
            d if GL_DRAWING.contains(&d) => {
                let mut s = self.state.borrow_mut();
                if s.unmodeled.is_none() {
                    s.unmodeled = Some(d.to_string());
                }
                Js::Undefined
            }
            _ => return None,
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- animation frames ------------------------------------------------------

/// How far apart animation frames fall: 60 a second.
pub const FRAME_NS: u64 = 16_666_667;

/// `performance`: `now()` reads the harness's monotonic clock, in
/// milliseconds.
pub struct Performance;

impl HostObject for Performance {
    fn class(&self) -> &'static str {
        "Performance"
    }
    fn get(&self, _: &str) -> Option<Js> {
        None
    }
    fn has_method(&self, name: &str) -> bool {
        name == "now"
    }
    fn call(&self, _: &Js, host: &mut Host, name: &str, _: &[Js]) -> Option<Result<Js, Js>> {
        (name == "now").then(|| Ok(Js::Num(host.monotonic_ns as f64 / 1e6)))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The globals the graphics add: the typed array constructors, `ImageData`,
/// `performance`, and `requestAnimationFrame` / `cancelAnimationFrame`,
/// whose callbacks run at the next frame, when the program waits
/// (`suspend`).
pub fn globals() -> Vec<(&'static str, Js)> {
    let mut out: Vec<(&'static str, Js)> = Kind::ALL
        .into_iter()
        .map(|kind| {
            (
                kind.name(),
                Js::function(move |_, _, args| TypedArray::construct(kind, args)),
            )
        })
        .collect();
    out.push((
        "ImageData",
        Js::function(|_, _, args| ImageData::construct(args)),
    ));
    out.push(("performance", Js::Host(Rc::new(Performance))));
    out.push((
        "requestAnimationFrame",
        Js::function(|w, _, args| {
            w.frame_ids += 1;
            let id = w.frame_ids;
            w.frames.push((id, arg(args, 0)));
            Ok(Js::Num(f64::from(id)))
        }),
    ));
    out.push((
        "cancelAnimationFrame",
        Js::function(|w, _, args| {
            let id = num(args, 0);
            w.frames.retain(|(k, _)| f64::from(*k) != id);
            Ok(Js::Undefined)
        }),
    ));
    out
}
