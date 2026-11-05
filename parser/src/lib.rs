use ansi_term::{Color, Style};
use chumsky::{prelude::*, text::keyword};
use convert_case::{Case, Casing};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDecl {
	pub name: String,
	pub alias: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protocol {
	pub name: String,
	pub imports: Vec<ImportDecl>,
	pub interfaces: Vec<(String, Interface)>,
	pub structs: Vec<(String, StructDef)>,
	pub enums: Vec<(String, EnumDef)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
	pub name: String,
	pub ty: Type,
	pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDef {
	pub name: String,
	pub doc: String,
	pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDef {
	pub name: String,
	pub doc: String,
	pub variants: Vec<EnumVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
	pub name: String,
	pub doc: Option<String>,
	pub fields: Vec<Field>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
	pub doc: String,
	pub methods: Vec<Method>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
	pub name: String,
	pub doc: Option<String>,
	pub params: Vec<Field>,
	/// if none, this is a oneway function.
	/// if some but vec is empty, this is a void function.
	pub returns: Option<Vec<Field>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomType {
	Named(String),
	Qualified(String, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
	Bool,
	U8,
	U16,
	U32,
	U64,
	I8,
	I16,
	I32,
	I64,
	F32,
	F64,
	String,
	Fd,
	Ref(Option<CustomType>), // binder object reference, optionally typed
	Custom(CustomType),      // reference to a struct, enum, or imported type by name
	Array(Box<Type>, u32),
	Vec(Box<Type>),
	Set(Box<Type>),
	Option(Box<Type>),
	Result(Box<Type>, Box<Type>),
	Map(Box<Type>, Box<Type>),
}

#[derive(Debug, Clone)]
pub struct ParseErrorEntry {
	pub line: usize,
	pub col: usize,
	pub source_line: String,
	pub found: String,
	pub expected: Vec<String>,
}

#[derive(Clone)]
pub struct ParseError {
	pub file: String,
	pub errors: Vec<ParseErrorEntry>,
}

impl std::fmt::Debug for ParseError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "\n{self}")
	}
}

impl std::fmt::Display for ParseError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let color = std::io::stderr().is_terminal();
		let red = if color {
			Color::Red.bold()
		} else {
			Style::new()
		};
		let blue = if color {
			Color::Blue.bold()
		} else {
			Style::new()
		};
		let white_bold = if color {
			Style::new().bold()
		} else {
			Style::new()
		};
		let pipe = blue.paint("|");

		for (i, entry) in self.errors.iter().enumerate() {
			if i > 0 {
				writeln!(f)?;
				writeln!(f)?;
			}
			let line_num = entry.line.to_string();
			let gutter = line_num.len();

			let expected = if entry.expected.is_empty() {
				"something else".to_string()
			} else if entry.expected.len() == 1 {
				entry.expected[0].clone()
			} else {
				let (last, rest) = entry.expected.split_last().unwrap();
				format!("{} or {last}", rest.join(", "))
			};

			// header
			writeln!(
				f,
				"{}: unexpected {}, expected {expected}",
				red.paint("error"),
				white_bold.paint(entry.found.as_str()),
			)?;

			// location
			writeln!(
				f,
				"{:>gutter$}{} {}:{}:{}",
				" ",
				blue.paint("-->"),
				self.file,
				entry.line,
				entry.col
			)?;

			// blank gutter
			writeln!(f, "{:>gutter$} {pipe}", " ")?;

			// source line
			writeln!(
				f,
				"{} {pipe} {}",
				blue.paint(line_num.as_str()),
				entry.source_line
			)?;

			// caret
			write!(
				f,
				"{:>gutter$} {pipe} {:>width$}",
				" ",
				red.paint("^"),
				width = entry.col
			)?;
		}
		Ok(())
	}
}

impl std::error::Error for ParseError {}

#[derive(Debug)]
pub enum LoadError {
	Io(std::io::Error),
	Parse(ParseError),
	CyclicImport(PathBuf),
	DuplicateAlias(String),
}

impl std::fmt::Display for LoadError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			LoadError::Io(e) => write!(f, "IO error: {e}"),
			LoadError::Parse(e) => write!(f, "Parse error:\n{e}"),
			LoadError::CyclicImport(p) => write!(f, "Cyclic import: {}", p.display()),
			LoadError::DuplicateAlias(a) => write!(f, "Duplicate import alias: {a}"),
		}
	}
}

impl std::error::Error for LoadError {}

/// Convert a byte offset in `src` to a (line, col) pair (both 1-based).
fn offset_to_line_col(src: &str, offset: usize) -> (usize, usize) {
	let mut line = 1;
	let mut col = 1;
	for (i, ch) in src.char_indices() {
		if i >= offset {
			break;
		}
		if ch == '\n' {
			line += 1;
			col = 1;
		} else {
			col += 1;
		}
	}
	(line, col)
}

fn rich_to_entry(src: &str, err: &Rich<'_, char>) -> ParseErrorEntry {
	let (line, col) = offset_to_line_col(src, err.span().start);
	let expected: Vec<String> = err
		.expected()
		.filter_map(|e| {
			let s = format!("{e}");
			// Filter out whitespace/comment internals leaked by the ws parser
			match s.as_str() {
				"' '" | "'\t'" | "'\n'" | "'\r'" | "'/' " | "'/'" | "something else"
				| "inline whitespace" => None,
				// Clean up double-quoted keywords like '"import"' → 'import'
				other => Some(
					other
						.strip_prefix("'\"")
						.and_then(|s| s.strip_suffix("\"'"))
						.map(|s| format!("'{s}'"))
						.unwrap_or_else(|| s),
				),
			}
		})
		.collect();
	let source_line = src.lines().nth(line - 1).unwrap_or("").to_string();

	ParseErrorEntry {
		line,
		col,
		source_line,
		found: err
			.found()
			.map(|c| match c {
				'\n' => "newline".to_string(),
				'\t' => "tab".to_string(),
				c => format!("'{c}'"),
			})
			.unwrap_or_else(|| "end of input".to_string()),
		expected,
	}
}

/// Derive the default alias from an import path.
/// Takes the filename, strips `.gluon`, splits by `.`, and takes the last segment.
pub fn default_alias(path: &str) -> String {
	let filename = path.rsplit('/').next().unwrap_or(path);
	let stem = filename.strip_suffix(".gluon").unwrap_or(filename);
	stem.rsplit('.')
		.next()
		.unwrap_or(stem)
		.to_string()
		.to_case(Case::Snake)
}

pub fn parser<'src>() -> impl Parser<'src, &'src str, Protocol, extra::Err<Rich<'src, char>>> {
	// --- Whitespace ---
	// Non-doc line comment: // but not /// (doc comment)
	let line_comment = just::<&str, &str, extra::Err<Rich<char>>>("//")
		.and_is(just("///").not())
		.ignore_then(none_of("\n\r").repeated())
		.ignored();

	// Whitespace consumer: spaces, tabs, newlines, and // comments (but not /// doc comments)
	let ws = choice((
		one_of(" \t\n\r").repeated().at_least(1).ignored(),
		line_comment,
	))
	.repeated()
	.ignored();

	// --- Import ---
	let import_path = none_of("\"")
		.repeated()
		.at_least(1)
		.to_slice()
		.map(|s: &str| s.to_string())
		.delimited_by(just('"'), just('"'));

	let import_stmt = keyword("import")
		.padded_by(ws)
		.ignore_then(import_path)
		.then(
			keyword("as")
				.padded_by(ws)
				.ignore_then(text::ident().map(str::to_string))
				.or_not(),
		)
		.map(|(name, alias_opt): (String, Option<String>)| {
			let alias = alias_opt.unwrap_or_else(|| default_alias(&name));
			ImportDecl { name, alias }
		});

	let imports = import_stmt
		.padded_by(ws)
		.repeated()
		.collect::<Vec<ImportDecl>>();

	// --- Doc comment ---
	// A doc line: optional horizontal whitespace, then ///, then content to end of line.
	// Newline (or end of input) terminates the line.
	let doc_line = text::inline_whitespace()
		.ignore_then(just("///"))
		.ignore_then(none_of("\n\r").repeated().to_slice().map(|s: &str| {
			let s = s.strip_prefix(' ').unwrap_or(s);
			s.trim_end().to_string()
		}))
		.then_ignore(text::newline().or(end()));

	let req_doc_block = doc_line
		.repeated()
		.at_least(1)
		.collect::<Vec<String>>()
		.map(|lines| lines.join("\n"))
		.labelled("doc comment (/// ...)");

	let opt_doc_block = doc_line.repeated().collect::<Vec<String>>().map(|lines| {
		if lines.is_empty() {
			None
		} else {
			Some(lines.join("\n"))
		}
	});

	// --- Type ---
	let type_parser = recursive(|p| {
		choice((
			keyword("bool").to(Type::Bool),
			keyword("u8").to(Type::U8),
			keyword("u16").to(Type::U16),
			keyword("u32").to(Type::U32),
			keyword("u64").to(Type::U64),
			keyword("i8").to(Type::I8),
			keyword("i16").to(Type::I16),
			keyword("i32").to(Type::I32),
			keyword("i64").to(Type::I64),
			keyword("f32").to(Type::F32),
			keyword("f64").to(Type::F64),
			keyword("String").to(Type::String),
			keyword("Fd").to(Type::Fd),
			keyword("Vec").ignore_then(
				p.clone()
					.map(|t: Type| Type::Vec(Box::new(t)))
					.delimited_by(just('<'), just('>')),
			),
			keyword("Set").ignore_then(
				p.clone()
					.map(|t: Type| Type::Set(Box::new(t)))
					.delimited_by(just('<'), just('>')),
			),
			keyword("Map").ignore_then(
				p.clone()
					.then_ignore(just(',').padded_by(ws))
					.then(p.clone())
					.map(|(k, v): (Type, Type)| Type::Map(Box::new(k), Box::new(v)))
					.delimited_by(just('<'), just('>')),
			),
			keyword("Option").ignore_then(
				p.clone()
					.map(|t: Type| Type::Option(Box::new(t)))
					.delimited_by(just('<'), just('>')),
			),
			keyword("Result").ignore_then(
				p.clone()
					.then_ignore(just(',').padded_by(ws))
					.then(p.clone())
					.map(|(k, v): (Type, Type)| Type::Result(Box::new(k), Box::new(v)))
					.delimited_by(just('<'), just('>')),
			),
			p.clone()
				.then_ignore(just(';').padded_by(ws))
				.then(text::int(10).map(|s: &str| s.parse::<u32>().unwrap()))
				.map(|(t, n): (Type, u32)| Type::Array(Box::new(t), n))
				.delimited_by(just('['), just(']')),
			keyword("Ref")
				.then(
					text::ident()
						.map(str::to_string)
						.then(
							just("::")
								.ignore_then(text::ident().map(str::to_string))
								.or_not(),
						)
						.map(|(first, second)| match second {
							Some(name) => CustomType::Qualified(first, name),
							None => CustomType::Named(first),
						})
						.delimited_by(just('<'), just('>'))
						.or_not(),
				)
				.map(|(_, name)| Type::Ref(name)),
			text::ident()
				.map(str::to_string)
				.then(
					just("::")
						.ignore_then(text::ident().map(str::to_string))
						.or_not(),
				)
				.map(|(first, second)| match second {
					Some(name) => Type::Custom(CustomType::Qualified(first, name)),
					None => Type::Custom(CustomType::Named(first)),
				}),
		))
		.padded_by(ws)
		.labelled("type")
	});

	// --- Params ---
	let param = text::ident()
		.map(str::to_string)
		.then_ignore(just(':').padded_by(ws))
		.then(type_parser.clone())
		.map(|(name, ty)| Field {
			name,
			ty,
			doc: None,
		});

	let params = param
		.separated_by(just(',').padded_by(ws))
		.collect::<Vec<Field>>()
		.delimited_by(just('(').padded_by(ws), just(')').padded_by(ws));

	let returns = just("->")
		.padded_by(ws)
		.ignore_then(params.clone())
		.or_not();

	// --- Method ---
	let method_name = text::ident().map(str::to_string).labelled("method");
	let method = opt_doc_block
		.padded_by(ws)
		.then(method_name)
		.then(params.clone())
		.then(returns)
		.map(|(((doc, name), params), returns)| Method {
			name,
			doc,
			params,
			returns,
		});
	let methods = method.padded_by(ws).repeated().collect::<Vec<Method>>();

	// --- Interface ---
	let interface = req_doc_block
		.then(
			keyword("interface")
				.padded_by(ws)
				.ignore_then(text::ident().map(str::to_string))
				.then(
					just('{')
						.padded_by(ws)
						.ignore_then(methods)
						.then_ignore(just('}').padded_by(ws)),
				),
		)
		.map(|(doc, (name, methods))| (name, Interface { doc, methods }));

	// --- Struct ---
	let struct_field = opt_doc_block
		.padded_by(ws)
		.then(
			text::ident()
				.map(str::to_string)
				.then_ignore(just(':').padded_by(ws))
				.then(type_parser.clone()),
		)
		.map(|(doc, (name, ty))| Field { name, ty, doc });
	let struct_fields = struct_field
		.separated_by(just(',').padded_by(ws))
		.allow_trailing()
		.collect::<Vec<Field>>();

	let struct_def = req_doc_block
		.then(
			keyword("struct")
				.padded_by(ws)
				.ignore_then(text::ident().map(str::to_string))
				.then(
					struct_fields
						.clone()
						.delimited_by(just('{'), just('}'))
						.padded_by(ws),
				),
		)
		.map(|(doc, (name, fields))| StructDef { name, doc, fields });

	// --- Enum ---
	let variant_fields = struct_fields
		.clone()
		.delimited_by(just('{'), just('}'))
		.padded_by(ws);
	let enum_variant = opt_doc_block
		.padded_by(ws)
		.then(
			text::ident()
				.map(str::to_string)
				.padded_by(ws)
				.then(variant_fields.or_not()),
		)
		.map(|(doc, (name, fields))| EnumVariant {
			name,
			doc,
			fields: fields.unwrap_or_default(),
		});
	let enum_variants = enum_variant
		.separated_by(just(',').padded_by(ws))
		.allow_trailing()
		.collect::<Vec<EnumVariant>>();

	let enum_def = req_doc_block
		.then(
			keyword("enum")
				.padded_by(ws)
				.ignore_then(text::ident().map(str::to_string))
				.then(
					enum_variants
						.delimited_by(just('{'), just('}'))
						.padded_by(ws),
				),
		)
		.map(|(doc, (name, variants))| EnumDef {
			name,
			doc,
			variants,
		});

	// --- Top-level ---
	enum TopLevel {
		Interface(String, Interface),
		Struct(StructDef),
		Enum(EnumDef),
	}

	let top_level_item = choice((
		interface.map(|(name, iface)| TopLevel::Interface(name, iface)),
		struct_def.map(TopLevel::Struct),
		enum_def.map(TopLevel::Enum),
	));

	imports
		.then(
			top_level_item
				.padded_by(ws)
				.repeated()
				.collect::<Vec<TopLevel>>(),
		)
		.then_ignore(end())
		.map(|(imports, items)| {
			let mut interfaces = Vec::new();
			let mut structs = Vec::new();
			let mut enums = Vec::new();
			for item in items {
				match item {
					TopLevel::Interface(name, iface) => {
						interfaces.push((name, iface));
					}
					TopLevel::Struct(s) => {
						structs.push((s.name.clone(), s));
					}
					TopLevel::Enum(e) => {
						enums.push((e.name.clone(), e));
					}
				}
			}
			Protocol {
				name: "".to_string(),
				imports,
				interfaces,
				structs,
				enums,
			}
		})
}

pub fn parse_idl(name: &str, input: &str) -> Result<Protocol, ParseError> {
	let mut protocol = parser()
		.parse(input)
		.into_result()
		.map_err(|errs| ParseError {
			file: name.to_string(),
			errors: errs.iter().map(|e| rich_to_entry(input, e)).collect(),
		})?;
	protocol.name = name.to_string();
	Ok(protocol)
}

#[test]
fn test_parse_idl() {
	let input = r#"
        /// A test interface
        interface Test {
            quit() // oneway transaction
            ping() -> () // transaction with empty reply, ensures it got it
            echo(input: String) -> (output: String) // just echo it back
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();
	assert_eq!(
		protocol,
		Protocol {
			name: "Test".to_string(),
			imports: vec![],
			interfaces: Vec::from([(
				"Test".to_string(),
				Interface {
					doc: "A test interface".to_string(),
					methods: vec![
						Method {
							name: "quit".to_string(),
							doc: None,
							params: vec![],
							returns: None,
						},
						Method {
							name: "ping".to_string(),
							doc: None,
							params: vec![],
							returns: Some(vec![]),
						},
						Method {
							name: "echo".to_string(),
							doc: None,
							params: vec![Field {
								name: "input".to_string(),
								ty: Type::String,
								doc: None
							}],
							returns: Some(vec![Field {
								name: "output".to_string(),
								ty: Type::String,
								doc: None
							}]),
						},
					],
				}
			),]),
			structs: Vec::new(),
			enums: Vec::new(),
		}
	);
}

#[test]
fn test_parse_types() {
	let input = r#"
        /// File storage interface
        interface FileStore {
            upload(data: Vec<u8>, name: String, executable: bool) -> (handle: Fd)
            read(handle: Fd, offset: u64, length: u32) -> (data: Vec<u8>, bytes_read: u32)
            hash(handle: Fd) -> (sha256: [u8; 32])
            stat(handle: Fd) -> (size: u64, mode: u16, uid: u32, gid: u32)
            set_tags(handle: Fd, tags: Set<String>) -> ()
            get_metadata(handle: Fd) -> (metadata: Map<String, String>)
            measure(handle: Fd) -> (temperature: f32, pressure: f64)
            adjust(handle: Fd, delta_x: i8, delta_y: i16, delta_z: i32, delta_w: i64) -> ()
        }
    "#;
	let protocol = parse_idl("FileStore", input).unwrap();
	let iface = &protocol
		.interfaces
		.iter()
		.find(|(name, _)| name == "FileStore")
		.unwrap()
		.1;
	assert_eq!(iface.methods.len(), 8);

	assert_eq!(
		iface.methods[0].params,
		vec![
			Field {
				name: "data".to_string(),
				ty: Type::Vec(Box::new(Type::U8)),
				doc: None
			},
			Field {
				name: "name".to_string(),
				ty: Type::String,
				doc: None
			},
			Field {
				name: "executable".to_string(),
				ty: Type::Bool,
				doc: None
			},
		]
	);
	assert_eq!(
		iface.methods[0].returns,
		Some(vec![Field {
			name: "handle".to_string(),
			ty: Type::Fd,
			doc: None
		}])
	);

	assert_eq!(
		iface.methods[1].params,
		vec![
			Field {
				name: "handle".to_string(),
				ty: Type::Fd,
				doc: None
			},
			Field {
				name: "offset".to_string(),
				ty: Type::U64,
				doc: None
			},
			Field {
				name: "length".to_string(),
				ty: Type::U32,
				doc: None
			},
		]
	);
	assert_eq!(
		iface.methods[1].returns,
		Some(vec![
			Field {
				name: "data".to_string(),
				ty: Type::Vec(Box::new(Type::U8)),
				doc: None
			},
			Field {
				name: "bytes_read".to_string(),
				ty: Type::U32,
				doc: None
			},
		])
	);

	assert_eq!(
		iface.methods[2].returns,
		Some(vec![Field {
			name: "sha256".to_string(),
			ty: Type::Array(Box::new(Type::U8), 32),
			doc: None
		}])
	);

	assert_eq!(
		iface.methods[3].returns,
		Some(vec![
			Field {
				name: "size".to_string(),
				ty: Type::U64,
				doc: None
			},
			Field {
				name: "mode".to_string(),
				ty: Type::U16,
				doc: None
			},
			Field {
				name: "uid".to_string(),
				ty: Type::U32,
				doc: None
			},
			Field {
				name: "gid".to_string(),
				ty: Type::U32,
				doc: None
			},
		])
	);

	assert_eq!(
		iface.methods[4].params[1],
		Field {
			name: "tags".to_string(),
			ty: Type::Set(Box::new(Type::String)),
			doc: None
		}
	);
	assert_eq!(iface.methods[4].returns, Some(vec![]));

	assert_eq!(
		iface.methods[5].returns,
		Some(vec![Field {
			name: "metadata".to_string(),
			ty: Type::Map(Box::new(Type::String), Box::new(Type::String)),
			doc: None,
		}])
	);

	assert_eq!(
		iface.methods[6].returns,
		Some(vec![
			Field {
				name: "temperature".to_string(),
				ty: Type::F32,
				doc: None
			},
			Field {
				name: "pressure".to_string(),
				ty: Type::F64,
				doc: None
			},
		])
	);

	assert_eq!(
		iface.methods[7].params,
		vec![
			Field {
				name: "handle".to_string(),
				ty: Type::Fd,
				doc: None
			},
			Field {
				name: "delta_x".to_string(),
				ty: Type::I8,
				doc: None
			},
			Field {
				name: "delta_y".to_string(),
				ty: Type::I16,
				doc: None
			},
			Field {
				name: "delta_z".to_string(),
				ty: Type::I32,
				doc: None
			},
			Field {
				name: "delta_w".to_string(),
				ty: Type::I64,
				doc: None
			},
		]
	);
	assert_eq!(iface.methods[7].returns, Some(vec![]));
}

#[test]
fn test_parse_ref_types() {
	let input = r#"
        /// Reference type tests
        interface RefTest {
            get_any() -> (obj: Ref)
            get_display(name: String) -> (display: Ref<Display>)
            bind(obj: Ref, iface: Ref<Compositor>) -> (bound: Ref)
        }
    "#;
	let protocol = parse_idl("RefTest", input).unwrap();
	let iface = &protocol
		.interfaces
		.iter()
		.find(|(name, _)| name == "RefTest")
		.unwrap()
		.1;

	assert_eq!(
		iface.methods[0].returns,
		Some(vec![Field {
			name: "obj".to_string(),
			ty: Type::Ref(None),
			doc: None
		}])
	);
	assert_eq!(
		iface.methods[1].returns,
		Some(vec![Field {
			name: "display".to_string(),
			ty: Type::Ref(Some(CustomType::Named("Display".into()))),
			doc: None
		}])
	);
	assert_eq!(
		iface.methods[2].params,
		vec![
			Field {
				name: "obj".to_string(),
				ty: Type::Ref(None),
				doc: None
			},
			Field {
				name: "iface".to_string(),
				ty: Type::Ref(Some(CustomType::Named("Compositor".into()))),
				doc: None
			},
		]
	);
	assert_eq!(
		iface.methods[2].returns,
		Some(vec![Field {
			name: "bound".to_string(),
			ty: Type::Ref(None),
			doc: None
		}])
	);
}

#[test]
fn test_parse_struct() {
	let input = r#"
        /// A 3D vector
        struct Vec3 {
            x: f32,
            y: f32,
            z: f32,
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();
	let s = &protocol
		.structs
		.iter()
		.find(|(name, _)| name == "Vec3")
		.unwrap()
		.1;
	assert_eq!(s.name, "Vec3");
	assert_eq!(s.doc, "A 3D vector");
	assert_eq!(
		s.fields,
		vec![
			Field {
				name: "x".to_string(),
				ty: Type::F32,
				doc: None
			},
			Field {
				name: "y".to_string(),
				ty: Type::F32,
				doc: None
			},
			Field {
				name: "z".to_string(),
				ty: Type::F32,
				doc: None
			},
		]
	);
}

#[test]
fn test_parse_enum() {
	let input = r#"
        /// Interaction methods
        enum Interaction {
            Pointer {
                origin: f32,
                direction: f32,
            },
            Hand {
                joints: u32,
            },
            Touch {
                point: f32,
            },
            None,
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();
	let e = &protocol
		.enums
		.iter()
		.find(|(name, _)| name == "Interaction")
		.unwrap()
		.1;
	assert_eq!(e.name, "Interaction");
	assert_eq!(e.doc, "Interaction methods");
	assert_eq!(e.variants.len(), 4);

	assert_eq!(e.variants[0].name, "Pointer");
	assert_eq!(
		e.variants[0].fields,
		vec![
			Field {
				name: "origin".to_string(),
				ty: Type::F32,
				doc: None
			},
			Field {
				name: "direction".to_string(),
				ty: Type::F32,
				doc: None
			},
		]
	);
	assert_eq!(e.variants[1].name, "Hand");
	assert_eq!(
		e.variants[1].fields,
		vec![Field {
			name: "joints".to_string(),
			ty: Type::U32,
			doc: None
		}]
	);
	assert_eq!(e.variants[2].name, "Touch");
	assert_eq!(
		e.variants[2].fields,
		vec![Field {
			name: "point".to_string(),
			ty: Type::F32,
			doc: None
		}]
	);
	assert_eq!(e.variants[3].name, "None");
	assert!(e.variants[3].fields.is_empty());
}

#[test]
fn test_parse_mixed() {
	let input = r#"
        /// A 3D vector
        struct Vec3 {
            x: f32,
            y: f32,
            z: f32,
        }

        /// Hit test result
        enum HitResult {
            Miss,
            Surface {
                point: Vec3,
                normal: Vec3,
                distance: f32,
            },
        }

        /// Spatial node interface
        interface Spatial {
            set_position(position: Vec3)
            interact(ray_origin: Vec3, ray_dir: Vec3) -> (hit: HitResult)
            get_child(name: String) -> (child: Ref<Spatial>)
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();

	assert!(protocol.structs.iter().any(|(name, _)| name == "Vec3"));
	assert!(protocol.enums.iter().any(|(name, _)| name == "HitResult"));
	assert!(
		protocol
			.interfaces
			.iter()
			.any(|(name, _)| name == "Spatial")
	);

	let iface = &protocol
		.interfaces
		.iter()
		.find(|(name, _)| name == "Spatial")
		.unwrap()
		.1;
	assert_eq!(iface.methods.len(), 3);
	assert_eq!(
		iface.methods[0].params,
		vec![Field {
			name: "position".to_string(),
			ty: Type::Custom(CustomType::Named("Vec3".into())),
			doc: None
		}]
	);
	assert_eq!(
		iface.methods[1].returns,
		Some(vec![Field {
			name: "hit".to_string(),
			ty: Type::Custom(CustomType::Named("HitResult".into())),
			doc: None
		}])
	);
	assert_eq!(
		iface.methods[2].returns,
		Some(vec![Field {
			name: "child".to_string(),
			ty: Type::Ref(Some(CustomType::Named("Spatial".into()))),
			doc: None
		}])
	);

	let hit = &protocol
		.enums
		.iter()
		.find(|(name, _)| name == "HitResult")
		.unwrap()
		.1;
	assert_eq!(
		hit.variants[1].fields[0],
		Field {
			name: "point".to_string(),
			ty: Type::Custom(CustomType::Named("Vec3".into())),
			doc: None
		}
	);
	assert_eq!(
		hit.variants[1].fields[1],
		Field {
			name: "normal".to_string(),
			ty: Type::Custom(CustomType::Named("Vec3".into())),
			doc: None
		}
	);
}

#[test]
fn test_doc_comments() {
	let input = r#"
        /// Position in 3D space
        /// with x, y, z components
        struct Vec3 {
            x: f32,
            y: f32,
            z: f32,
        }

        /// Input interaction type
        enum InputType {
            None,
            Pointer {
                origin: Vec3,
                direction: Vec3,
            },
            Touch,
        }

        /// Spatial node
        interface Node {
            set_position(pos: Vec3)
            get_name() -> (name: String)
        }
    "#;
	let protocol = parse_idl("DocTest", input).unwrap();

	// Multi-line struct doc
	let s = &protocol
		.structs
		.iter()
		.find(|(name, _)| name == "Vec3")
		.unwrap()
		.1;
	assert_eq!(s.doc, "Position in 3D space\nwith x, y, z components");

	// Enum doc
	let e = &protocol
		.enums
		.iter()
		.find(|(name, _)| name == "InputType")
		.unwrap()
		.1;
	assert_eq!(e.doc, "Input interaction type");

	// Interface doc
	let iface = &protocol
		.interfaces
		.iter()
		.find(|(name, _)| name == "Node")
		.unwrap()
		.1;
	assert_eq!(iface.doc, "Spatial node");
}

#[test]
fn test_field_and_variant_docs() {
	let input = r#"
        /// A color
        struct Color {
            /// Red channel
            r: f32,
            /// Green channel
            g: f32,
            /// Blue channel
            b: f32,
            a: f32,
        }

        /// Paint types
        enum Paint {
            /// No paint
            None,
            /// Solid color fill
            Solid {
                /// The fill color
                color: Color,
            },
            Gradient {
                start: Color,
                end: Color,
            },
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();

	let s = &protocol
		.structs
		.iter()
		.find(|(name, _)| name == "Color")
		.unwrap()
		.1;
	assert_eq!(
		s.fields[0],
		Field {
			name: "r".to_string(),
			ty: Type::F32,
			doc: Some("Red channel".to_string())
		}
	);
	assert_eq!(
		s.fields[1],
		Field {
			name: "g".to_string(),
			ty: Type::F32,
			doc: Some("Green channel".to_string())
		}
	);
	assert_eq!(
		s.fields[2],
		Field {
			name: "b".to_string(),
			ty: Type::F32,
			doc: Some("Blue channel".to_string())
		}
	);
	assert_eq!(
		s.fields[3],
		Field {
			name: "a".to_string(),
			ty: Type::F32,
			doc: None
		}
	); // no doc

	let e = &protocol
		.enums
		.iter()
		.find(|(name, _)| name == "Paint")
		.unwrap()
		.1;
	assert_eq!(e.variants[0].name, "None");
	assert_eq!(e.variants[0].doc, Some("No paint".to_string()));
	assert_eq!(e.variants[1].name, "Solid");
	assert_eq!(e.variants[1].doc, Some("Solid color fill".to_string()));
	assert_eq!(
		e.variants[1].fields[0],
		Field {
			name: "color".to_string(),
			ty: Type::Custom(CustomType::Named("Color".into())),
			doc: Some("The fill color".to_string())
		}
	);
	assert_eq!(e.variants[2].name, "Gradient");
	assert_eq!(e.variants[2].doc, None); // no doc
	assert_eq!(
		e.variants[2].fields[0],
		Field {
			name: "start".to_string(),
			ty: Type::Custom(CustomType::Named("Color".into())),
			doc: None
		}
	);
}

#[test]
fn test_mixed_doc_comments() {
	let input = r#"
        /// A point in space
        struct Point {
            /// X coordinate
            x: f32,
            y: f32,
            /// Z coordinate
            z: f32,
        }

        /// Shape types
        enum Shape {
            /// Nothing to render
            None,
            Circle {
                radius: f32,
            },
            /// A rectangle shape
            Rect {
                /// Width of the rectangle
                width: f32,
                height: f32,
            },
        }

        /// Drawing interface
        interface Canvas {
            /// Clear the canvas
            clear()
            reset()
            /// Draw a shape at a position
            /// with the given color
            draw(pos: Point, shape: Shape) -> (id: u32)
            remove(id: u32) -> ()
        }
    "#;
	assert_eq!(
		parse_idl("Test", input).unwrap(),
		Protocol {
			name: "Test".to_string(),
			imports: vec![],
			structs: vec![(
				"Point".to_string(),
				StructDef {
					name: "Point".to_string(),
					doc: "A point in space".to_string(),
					fields: vec![
						Field {
							name: "x".to_string(),
							ty: Type::F32,
							doc: Some("X coordinate".to_string())
						},
						Field {
							name: "y".to_string(),
							ty: Type::F32,
							doc: None
						},
						Field {
							name: "z".to_string(),
							ty: Type::F32,
							doc: Some("Z coordinate".to_string())
						},
					],
				},
			)],
			enums: vec![(
				"Shape".to_string(),
				EnumDef {
					name: "Shape".to_string(),
					doc: "Shape types".to_string(),
					variants: vec![
						EnumVariant {
							name: "None".to_string(),
							doc: Some("Nothing to render".to_string()),
							fields: vec![],
						},
						EnumVariant {
							name: "Circle".to_string(),
							doc: None,
							fields: vec![Field {
								name: "radius".to_string(),
								ty: Type::F32,
								doc: None
							},],
						},
						EnumVariant {
							name: "Rect".to_string(),
							doc: Some("A rectangle shape".to_string()),
							fields: vec![
								Field {
									name: "width".to_string(),
									ty: Type::F32,
									doc: Some("Width of the rectangle".to_string())
								},
								Field {
									name: "height".to_string(),
									ty: Type::F32,
									doc: None
								},
							],
						},
					],
				},
			)],
			interfaces: vec![(
				"Canvas".to_string(),
				Interface {
					doc: "Drawing interface".to_string(),
					methods: vec![
						Method {
							name: "clear".to_string(),
							doc: Some("Clear the canvas".to_string()),
							params: vec![],
							returns: None,
						},
						Method {
							name: "reset".to_string(),
							doc: None,
							params: vec![],
							returns: None,
						},
						Method {
							name: "draw".to_string(),
							doc: Some(
								"Draw a shape at a position\nwith the given color".to_string()
							),
							params: vec![
								Field {
									name: "pos".to_string(),
									ty: Type::Custom(CustomType::Named("Point".into())),
									doc: None
								},
								Field {
									name: "shape".to_string(),
									ty: Type::Custom(CustomType::Named("Shape".into())),
									doc: None
								},
							],
							returns: Some(vec![Field {
								name: "id".to_string(),
								ty: Type::U32,
								doc: None
							},]),
						},
						Method {
							name: "remove".to_string(),
							doc: None,
							params: vec![Field {
								name: "id".to_string(),
								ty: Type::U32,
								doc: None
							},],
							returns: Some(vec![]),
						},
					],
				},
			)],
		}
	);
}

#[test]
fn test_missing_required_doc() {
	assert!(parse_idl("Test", "struct Foo { x: u32 }").is_err());
	assert!(parse_idl("Test", "enum Bar { A, B }").is_err());
	assert!(parse_idl("Test", "interface Baz { ping() -> () }").is_err());
}

#[test]
fn test_error_display() {
	let cases = &[
		("missing doc", "struct Foo { x: u32 }"),
		("bad token", "/// doc\ninterface Foo {\n  123bad\n}"),
		("unclosed brace", "/// doc\nstruct Foo {\n  x: u32,\n"),
		("bad type", "/// doc\nstruct Foo {\n  x: ???,\n}"),
	];
	for (label, input) in cases {
		let err = parse_idl("Test", input).unwrap_err();
		println!("--- {label} ---\n{err}\n");
	}
}

/// Load a `.gluon` protocol file from disk, recursively resolving imports.
pub fn load_protocol(path: &Path) -> Result<Protocol, LoadError> {
	let mut seen = std::collections::HashSet::new();
	let mut cache = std::collections::HashMap::new();
	load_protocol_inner(path, &mut seen, &mut cache)
}

fn load_protocol_inner(
	path: &Path,
	seen: &mut std::collections::HashSet<PathBuf>,
	cache: &mut std::collections::HashMap<PathBuf, Protocol>,
) -> Result<Protocol, LoadError> {
	let canonical = path.canonicalize().map_err(LoadError::Io)?;

	if let Some(cached) = cache.get(&canonical) {
		return Ok(cached.clone());
	}

	if !seen.insert(canonical.clone()) {
		return Err(LoadError::CyclicImport(canonical));
	}

	let source = std::fs::read_to_string(&canonical).map_err(LoadError::Io)?;
	let name = default_alias(canonical.to_str().unwrap_or(""));
	let protocol = parse_idl(&name, &source).map_err(LoadError::Parse)?;

	// Check for duplicate aliases
	let mut alias_set = std::collections::HashSet::new();
	for import in &protocol.imports {
		if !alias_set.insert(import.alias.clone()) {
			return Err(LoadError::DuplicateAlias(import.alias.clone()));
		}
	}

	// Recursively load imported protocols
	let parent_dir = canonical.parent().unwrap_or(Path::new("."));
	for import in &protocol.imports {
		let import_path = parent_dir.join(&import.name);
		load_protocol_inner(&import_path, seen, cache)?;
	}

	seen.remove(&canonical);
	cache.insert(canonical, protocol.clone());
	Ok(protocol)
}

// --- Import system tests ---

#[test]
fn test_default_alias() {
	assert_eq!(default_alias("spatial.gluon"), "spatial");
	assert_eq!(default_alias("org.stardustxr.spatial.gluon"), "spatial");
	assert_eq!(default_alias("relative/path/types.gluon"), "types");
	assert_eq!(default_alias("a/b/org.stardustxr.spatial.gluon"), "spatial");
}

#[test]
fn test_parse_imports() {
	let input = r#"
        import "spatial.gluon"
        import "other.gluon" as myalias

        /// Test interface
        interface Foo {
            do_thing() -> ()
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();
	assert_eq!(protocol.imports.len(), 2);
	assert_eq!(protocol.imports[0].name, "spatial.gluon");
	assert_eq!(protocol.imports[0].alias, "spatial");
	assert_eq!(protocol.imports[1].name, "other.gluon");
	assert_eq!(protocol.imports[1].alias, "myalias");
}

#[test]
fn test_parse_qualified_type_in_params() {
	let input = r#"
        import "spatial.gluon"

        /// Test interface
        interface Foo {
            do_thing(pos: spatial::Vec3) -> (result: spatial::HitResult)
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();
	let iface = &protocol
		.interfaces
		.iter()
		.find(|(name, _)| name == "Foo")
		.unwrap()
		.1;
	assert_eq!(
		iface.methods[0].params[0].ty,
		Type::Custom(CustomType::Qualified("spatial".into(), "Vec3".into()))
	);
	assert_eq!(
		iface.methods[0].returns.as_ref().unwrap()[0].ty,
		Type::Custom(CustomType::Qualified("spatial".into(), "HitResult".into()))
	);
}

#[test]
fn test_parse_qualified_type_in_containers() {
	let input = r#"
        import "spatial.gluon"

        /// Test interface
        interface Foo {
            get_all() -> (items: Vec<spatial::Vec3>)
            get_map() -> (m: Map<String, spatial::Vec3>)
            get_arr() -> (a: [spatial::Vec3; 4])
            get_ref() -> (r: Ref<spatial::Node>)
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();
	let iface = &protocol
		.interfaces
		.iter()
		.find(|(name, _)| name == "Foo")
		.unwrap()
		.1;

	assert_eq!(
		iface.methods[0].returns.as_ref().unwrap()[0].ty,
		Type::Vec(Box::new(Type::Custom(CustomType::Qualified(
			"spatial".into(),
			"Vec3".into()
		))))
	);
	assert_eq!(
		iface.methods[1].returns.as_ref().unwrap()[0].ty,
		Type::Map(
			Box::new(Type::String),
			Box::new(Type::Custom(CustomType::Qualified(
				"spatial".into(),
				"Vec3".into()
			)))
		)
	);
	assert_eq!(
		iface.methods[2].returns.as_ref().unwrap()[0].ty,
		Type::Array(
			Box::new(Type::Custom(CustomType::Qualified(
				"spatial".into(),
				"Vec3".into()
			))),
			4
		)
	);
	assert_eq!(
		iface.methods[3].returns.as_ref().unwrap()[0].ty,
		Type::Ref(Some(CustomType::Qualified("spatial".into(), "Node".into())))
	);
}

#[test]
fn test_parse_qualified_type_in_struct() {
	let input = r#"
        import "spatial.gluon"

        /// A transform
        struct Transform {
            position: spatial::Vec3,
            rotation: spatial::Quat,
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();
	let s = &protocol
		.structs
		.iter()
		.find(|(name, _)| name == "Transform")
		.unwrap()
		.1;
	assert_eq!(
		s.fields[0].ty,
		Type::Custom(CustomType::Qualified("spatial".into(), "Vec3".into()))
	);
	assert_eq!(
		s.fields[1].ty,
		Type::Custom(CustomType::Qualified("spatial".into(), "Quat".into()))
	);
}

#[test]
fn test_parse_builtin_prefixed_names() {
	let input = r#"
        /// Names that start with a builtin type
        struct Holder {
            a: FdHolder,
            b: Reference,
            c: Stringy,
            d: Vector,
            e: Optional,
            f: u8x,
            g: Fd,
            h: Ref<Reference>,
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();
	let s = &protocol.structs[0].1;
	let named = |n: &str| Type::Custom(CustomType::Named(n.into()));
	assert_eq!(s.fields[0].ty, named("FdHolder"));
	assert_eq!(s.fields[1].ty, named("Reference"));
	assert_eq!(s.fields[2].ty, named("Stringy"));
	assert_eq!(s.fields[3].ty, named("Vector"));
	assert_eq!(s.fields[4].ty, named("Optional"));
	assert_eq!(s.fields[5].ty, named("u8x"));
	assert_eq!(s.fields[6].ty, Type::Fd);
	assert_eq!(
		s.fields[7].ty,
		Type::Ref(Some(CustomType::Named("Reference".into())))
	);
}

#[test]
fn test_parse_qualified_type_in_enum() {
	let input = r#"
        import "spatial.gluon"

        /// Hit result
        enum HitResult {
            Miss,
            Hit {
                point: spatial::Vec3,
            },
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();
	let e = &protocol
		.enums
		.iter()
		.find(|(name, _)| name == "HitResult")
		.unwrap()
		.1;
	assert_eq!(
		e.variants[1].fields[0].ty,
		Type::Custom(CustomType::Qualified("spatial".into(), "Vec3".into()))
	);
}

#[test]
fn test_load_protocol_basic() {
	use std::io::Write;
	let dir = tempfile::tempdir().unwrap();

	// Write spatial.gluon
	let spatial_path = dir.path().join("spatial.gluon");
	let mut f = std::fs::File::create(&spatial_path).unwrap();
	write!(
		f,
		"/// A 3D vector\nstruct Vec3 {{\n    x: f32,\n    y: f32,\n    z: f32,\n}}\n"
	)
	.unwrap();

	// Write main.gluon that imports spatial
	let main_path = dir.path().join("main.gluon");
	let mut f = std::fs::File::create(&main_path).unwrap();
	write!(
        f,
        "import \"spatial.gluon\"\n\n/// Main interface\ninterface Main {{\n    move_to(pos: spatial::Vec3)\n}}\n"
    )
    .unwrap();

	let protocol = load_protocol(&main_path).unwrap();
	assert_eq!(protocol.imports.len(), 1);
	assert_eq!(protocol.imports[0].alias, "spatial");

	let iface = &protocol
		.interfaces
		.iter()
		.find(|(name, _)| name == "Main")
		.unwrap()
		.1;
	assert_eq!(
		iface.methods[0].params[0].ty,
		Type::Custom(CustomType::Qualified("spatial".into(), "Vec3".into()))
	);
}

#[test]
fn test_load_protocol_cyclic_import() {
	use std::io::Write;
	let dir = tempfile::tempdir().unwrap();

	let a_path = dir.path().join("a.gluon");
	let b_path = dir.path().join("b.gluon");

	let mut f = std::fs::File::create(&a_path).unwrap();
	write!(
		f,
		"import \"b.gluon\"\n\n/// A\ninterface A {{\n    ping() -> ()\n}}\n"
	)
	.unwrap();

	let mut f = std::fs::File::create(&b_path).unwrap();
	write!(
		f,
		"import \"a.gluon\"\n\n/// B\ninterface B {{\n    pong() -> ()\n}}\n"
	)
	.unwrap();

	let result = load_protocol(&a_path);
	assert!(matches!(result, Err(LoadError::CyclicImport(_))));
}

#[test]
fn test_load_protocol_file_not_found() {
	let result = load_protocol(Path::new("/nonexistent/path/test.gluon"));
	assert!(matches!(result, Err(LoadError::Io(_))));
}

#[test]
fn test_load_protocol_duplicate_alias() {
	use std::io::Write;
	let dir = tempfile::tempdir().unwrap();

	let types_path = dir.path().join("types.gluon");
	let mut f = std::fs::File::create(&types_path).unwrap();
	write!(f, "/// A type\nstruct Foo {{\n    x: u32,\n}}\n").unwrap();

	let other_dir = dir.path().join("sub");
	std::fs::create_dir(&other_dir).unwrap();
	let other_types_path = other_dir.join("types.gluon");
	let mut f = std::fs::File::create(&other_types_path).unwrap();
	write!(f, "/// Another type\nstruct Bar {{\n    y: u32,\n}}\n").unwrap();

	let main_path = dir.path().join("main.gluon");
	let mut f = std::fs::File::create(&main_path).unwrap();
	write!(
        f,
        "import \"types.gluon\"\nimport \"sub/types.gluon\"\n\n/// Main\ninterface Main {{\n    ping() -> ()\n}}\n"
    )
    .unwrap();

	let result = load_protocol(&main_path);
	assert!(matches!(result, Err(LoadError::DuplicateAlias(_))));
}

#[test]
fn test_load_protocol_diamond_import() {
	use std::io::Write;
	let dir = tempfile::tempdir().unwrap();

	// shared.gluon
	let shared_path = dir.path().join("shared.gluon");
	let mut f = std::fs::File::create(&shared_path).unwrap();
	write!(
		f,
		"/// Shared type\nstruct Vec3 {{\n    x: f32,\n    y: f32,\n    z: f32,\n}}\n"
	)
	.unwrap();

	// a.gluon imports shared
	let a_path = dir.path().join("a.gluon");
	let mut f = std::fs::File::create(&a_path).unwrap();
	write!(
		f,
		"import \"shared.gluon\"\n\n/// A\ninterface A {{\n    get_pos() -> (pos: shared::Vec3)\n}}\n"
	)
	.unwrap();

	// b.gluon imports shared
	let b_path = dir.path().join("b.gluon");
	let mut f = std::fs::File::create(&b_path).unwrap();
	write!(
		f,
		"import \"shared.gluon\"\n\n/// B\ninterface B {{\n    set_pos(pos: shared::Vec3)\n}}\n"
	)
	.unwrap();

	// main.gluon imports both a and b
	let main_path = dir.path().join("main.gluon");
	let mut f = std::fs::File::create(&main_path).unwrap();
	write!(
		f,
		"import \"a.gluon\"\nimport \"b.gluon\"\n\n/// Main\ninterface Main {{\n    ping() -> ()\n}}\n"
	)
	.unwrap();
}

#[test]
fn test_parse_no_imports() {
	let input = r#"
        /// Test interface
        interface Foo {
            ping() -> ()
        }
    "#;
	let protocol = parse_idl("Test", input).unwrap();
	assert!(protocol.imports.is_empty());
}

// #[test]
// fn test_parse_real_interface() {
//     let input = include_str!("../test_idl/org.stardustxr.gluon.test.gluon");
//     let protocol = parse_idl("test", input).unwrap();
//     assert!(protocol.imports.is_empty());
//     assert!(protocol.imported_protocols.is_empty());
// }

#[test]
fn test_empty_interface() {
	let input = r#"
        /// An empty interface
        interface Empty {
        }
    "#;
	let protocol = parse_idl("Empty", input).unwrap();
	assert_eq!(
		protocol,
		Protocol {
			name: "Empty".to_string(),
			imports: vec![],
			interfaces: Vec::from([(
				"Empty".to_string(),
				Interface {
					doc: "An empty interface".to_string(),
					methods: vec![],
				},
			)]),
			structs: Vec::new(),
			enums: Vec::new(),
		}
	);
}
