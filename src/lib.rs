use jotdown::Render;
use proc_macro2::TokenStream;
use quote::quote;
use serde::Deserialize;
use std::{env, fs::{self, File}, io, io::Write as _, path::{Path, PathBuf}};
use tera::Tera;
use walkdir::{DirEntry, WalkDir};
use yaml_front_matter::{Document, YamlFrontMatter};

#[derive(Deserialize)]
struct DjotMetadata {
	title: Option<String>,
	layout: Option<String>,
}

fn render_djot(input: &str) -> Result<String, String> {
	let doc: Document<DjotMetadata> = YamlFrontMatter::parse::<DjotMetadata>(input).map_err(|e| format!("frontmatter parsing failed: {e}"))?;

	let mut result = String::new();

	if let Some(layout) = doc.metadata.layout {
		result.push_str(&format!(r#"{{% extends "{layout}.html" %}}"#));
		result.push_str("\n");
	}

	if let Some(title) = doc.metadata.title {
		result.push_str(&format!("{{% block headtitle %}}{title}{{% endblock headtitle %}}"));
		result.push_str(&format!("{{% block pagetitle %}}{title}{{% endblock pagetitle %}}"));
		result.push_str("\n");
	}

	result.push_str("{% block content %}\n");
	let events = jotdown::Parser::new(&doc.content);
	jotdown::html::Renderer::default().push(events, &mut result).map_err(|e| format!("djot rendering failed: {e}"))?;
	result.push_str("{% endblock content %}\n");

	Ok(result)
}

pub struct Resource {
	source: PathBuf,
	path: PathBuf,
}

impl Resource {
	pub fn new(source: PathBuf, path: PathBuf) -> Self {
		Resource { source, path }
	}

	pub fn source(&self) -> PathBuf {
		self.source.clone()
	}

	pub fn paths(&self) -> Vec<PathBuf> {
		let mut path = self.path.clone();

		if let Some("dj") = path.extension().map(|v| v.to_str().unwrap()) {
			path.set_extension("html");
		}

		if let Some("index.html") = path.file_name().map(|v| v.to_str().unwrap()) {
			if path == PathBuf::from("/index.html") {
				vec![path.clone(), PathBuf::from("/")]
			} else {
				vec![path.clone(), path.parent().unwrap().to_path_buf(), path.parent().unwrap().join("")]
			}
		} else {
			vec![path]
		}
	}

	pub fn content(&self) -> Vec<u8> {
		let mut templater = Tera::new("layouts/**/*.html").unwrap();
		let empty_render_ctx = tera::Context::new();

		match self.source.extension().map(|v| v.to_str().unwrap()) {
			Some("html") => templater.render_str(&fs::read_to_string(&self.source).unwrap(), &empty_render_ctx).unwrap().into(),
			Some("dj") => templater.render_str(&render_djot(&fs::read_to_string(&self.source).unwrap()).unwrap(), &empty_render_ctx).unwrap().into(),
			_ => fs::read(&self.source).unwrap(),
		}
	}

	pub fn media_type(&self) -> TokenStream {
		match self.source.extension().map(|v| v.to_str().unwrap()) {
			Some("html") | Some("dj") => quote! { ::mime::TEXT_HTML_UTF_8 },
			Some("css") => quote! { ::mime::TEXT_CSS },
			Some("cer") => quote! { "application/pkix-cert".parse::<::mime::Mime>().unwrap() },
			Some("der") => quote! { ::mime::APPLICATION_OCTET_STREAM },
			Some("gpg") => quote! { "application/pgp-keys".parse::<::mime::Mime>().unwrap() },
			Some("ico") => quote! { "image/vnd.microsoft.icon".parse::<::mime::Mime>().unwrap() },
			Some("js")  => quote! { ::mime::APPLICATION_JSON },
			Some("pem") => quote! { ::mime::TEXT_PLAIN },
			Some("pkbf") => quote! { ::mime::APPLICATION_OCTET_STREAM },
			Some("png") => quote! { ::mime::IMAGE_PNG },
			Some("txt") => quote! { ::mime::TEXT_PLAIN },
			Some(ext) => panic!("Unmimeable file extension: {ext:?}"),
			None      => quote! { ::mime::APPLICATION_OCTET_STREAM },
		}
	}
}

fn scan_resources<P>(base_path: P) -> Vec<Resource> where P: AsRef<Path> {
	let mut resources: Vec<Resource> = vec![];

	fn valid_static_file(entry: &DirEntry) -> bool {
		!entry.file_name()
			.to_str()
			.map(|s| s.starts_with("."))
			.unwrap_or(false)
	}

	for entry in WalkDir::new(&base_path).into_iter().filter_entry(valid_static_file) {
		let entry = entry.unwrap();
		dbg!(&entry);

		if env::var("PROFILE").unwrap() == "release" || entry.file_type().is_dir() {
			println!("cargo::rerun-if-changed={}", entry.path().display());
		}

		if entry.file_type().is_file() {
			resources.push(Resource::new(entry.path().to_path_buf(), PathBuf::from("/").join(entry.path().to_path_buf().strip_prefix(&base_path).unwrap())));
		}
	}

	resources
}

pub fn write_static_content_module<P>(fd: &mut File, base_path: P) -> Result<(), io::Error> where P: AsRef<Path> {
	let resources = scan_resources(base_path);
	let mut resource_paths = vec![];
	let mut resource_responses = vec![];

	for r in resources {
		let source = r.source().display().to_string();
		let content = r.content();
		let media_type = r.media_type();

		for p in r.paths() {
			let path = p.display().to_string();

			if env::var("PROFILE").unwrap() == "release" {
				resource_responses.push(
					quote! {
						#path => ::actix_web::HttpResponse::Ok()
							.insert_header(::actix_web::http::header::ContentType(#media_type))
								.body(vec![#(#content),*]),
					}
				);
			} else {
				resource_responses.push(
					quote! {
						#path => {
							let r = ::static_builder::Resource::new(::std::path::PathBuf::from(#source), ::std::path::PathBuf::from(#path));

							::actix_web::HttpResponse::Ok()
							.insert_header(::actix_web::http::header::ContentType(#media_type))
								.body(r.content())
						},
					}
				);
			}

			resource_paths.push(path);
		}
	}

	let quoted_code = quote! {
		pub(crate) struct StaticContent;

		impl StaticContent {
			#[allow(clippy::panic, clippy::unwrap_used)]  // Things that go wrong in here are worth exploding for
			#[allow(clippy::too_many_lines)]  // Autogenerated code has different notions of style
			fn response(path: &str) -> ::actix_web::HttpResponse {
				match path {
					#(#resource_responses)*
					p => panic!("Where the heck did we get {p} from?!?"),
				}
			}
		}

		impl ::actix_web::dev::HttpServiceFactory for StaticContent {
			fn register(self, config: &mut ::actix_web::dev::AppService) {
				let mut res_def = ::actix_web::dev::ResourceDef::new(vec![#(#resource_paths),*]);
				res_def.set_name("StaticContent");

				config.register_service(res_def, None, self, None);
			}
		}

		impl ::actix_web::dev::ServiceFactory<::actix_web::dev::ServiceRequest> for StaticContent {
			type Response = ::actix_web::dev::ServiceResponse;
			type Error = ::actix_web::Error;
			type InitError = ();
			type Config = ();
			type Service = StaticContent;
			type Future = ::std::future::Ready<Result<Self::Service, ()>>;

			fn new_service(&self, _cfg: Self::Config) -> Self::Future {
				::std::future::ready(Ok(StaticContent))
			}
		}

		impl ::actix_web::dev::Service<::actix_web::dev::ServiceRequest> for StaticContent {
			type Response = ::actix_web::dev::ServiceResponse;
			type Error = ::actix_web::Error;
			type Future = ::std::future::Ready<Result<Self::Response, Self::Error>>;

			::actix_web::dev::always_ready!();

			fn call(&self, req: ::actix_web::dev::ServiceRequest) -> Self::Future {
				if !matches!(*req.method(), ::actix_web::http::Method::HEAD | ::actix_web::http::Method::GET) {
					return ::std::future::ready(Ok(req.into_response(::actix_web::HttpResponse::MethodNotAllowed())));
				}

				let res = StaticContent::response(req.path());
				::std::future::ready(Ok(req.into_response(res)))
			}
		}
	};
	let syntax_tree = syn::parse2(quoted_code).unwrap();
	writeln!(fd, "{}", prettyplease::unparse(&syntax_tree))?;

	Ok(())
}
