//! Installing the HTML shims as the document's own default show rules.
//!
//! Typst's HTML export drops layout elements — `align`, `place`, `grid` and the
//! rest — along with everything inside them, so a document that centres its
//! title has no title at all in HTML. The rules that put them back are written
//! in Typst, in `static/html/shims.typ`.
//!
//! They were previously installed by compiling the document through a generated
//! wrapper file that applied them and then included the document. That made the
//! wrapper the compile's main file, so source ranges referred to a file the
//! reader does not have, and every consumer needed a second lookup to find the
//! real document.
//!
//! They are now installed as styles. The shim file is evaluated once as a
//! module, its exported rules are converted to show recipes, and the recipes are
//! added to the library the document is compiled against. The document remains
//! the main file, and its own show rules apply inside these.

use typst::World;
use typst::diag::SourceResult;
use typst::foundations::{Func, FromValue, Recipe, Selector, Style, Transformation, Value};
use typst::syntax::package::{PackageSpec, PackageVersion};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::utils::LazyHash;
use typst::{Feature, Features, Library, LibraryExt};

/// Where the shim module sits, as far as the compiler is concerned.
///
/// Addressed as a package rather than as a path in the project, so that it does
/// not appear in the user's directory and cannot be mistaken for one of their
/// files.
pub fn shims_id() -> FileId {
    let package = PackageSpec {
        namespace: "talimist".into(),
        name: "html-shims".into(),
        version: PackageVersion {
            major: 0,
            minor: 1,
            patch: 0,
        },
    };
    let vpath = VirtualPath::new("shims.typ").expect("shims.typ is a valid path");
    RootedPath::new(VirtualRoot::Package(package), vpath).intern()
}

/// The shim module's source, as the compiler will read it.
pub fn shims_source() -> Source {
    Source::new(shims_id(), super::html::shims_typ().into())
}

/// The library the shims are built on top of.
///
/// HTML export is behind a feature flag, and the compile pipeline replaces the
/// library with a fresh one when it is asked for HTML and the flag is not set.
/// Enabling it here keeps the library that has the shims in it.
pub fn base_library(inputs: std::sync::Arc<LazyHash<typst::foundations::Dict>>) -> LazyHash<Library> {
    let library = Library::builder()
        .with_inputs((**inputs).clone())
        .with_features(Features::from_iter([Feature::Html]))
        .build();
    LazyHash::new(library)
}

/// The library a document should be compiled against to have the shims.
///
/// Built from `base`, so inputs and features are unchanged: this adds show
/// recipes and nothing else.
pub fn library_with_shims(
    world: &dyn World,
    base: &LazyHash<Library>,
) -> Result<Library, String> {
    let source = shims_source();
    let module = evaluate(world, base, &source)
        .map_err(|errors| match errors.first() {
            Some(error) => format!("the HTML shims did not compile: {}", error.message),
            None => "the HTML shims did not compile".to_owned(),
        })?;

    let rules = module
        .scope()
        .get("rules")
        .map(|binding| binding.read().clone())
        .ok_or("the HTML shims export no rules")?;
    let Value::Array(rules) = rules else {
        return Err("the HTML shims' rules are not a list".into());
    };

    let mut library = base.clone().into_inner();
    for rule in rules.iter() {
        let Value::Array(pair) = rule else {
            return Err("a shim rule is not a (selector, function) pair".into());
        };
        let (Ok(selector), Ok(transform)) = (pair.at(0, None), pair.at(1, None)) else {
            return Err("a shim rule is not a (selector, function) pair".into());
        };
        let selector = Selector::from_value(selector)
            .map_err(|err| format!("a shim rule has no selector: {}", err.message()))?;
        let transform = Func::from_value(transform)
            .map_err(|err| format!("a shim rule has no function: {}", err.message()))?;
        // The function's own span, so that an error raised inside a rule is
        // reported at the rule.
        let span = transform.span();
        library.styles.push(Style::Recipe(Recipe::new(
            Some(selector),
            Transformation::Func(transform),
            span,
        )));
    }

    Ok(library)
}

/// Evaluates the shim module in a world of the caller's choosing.
fn evaluate(world: &dyn World, library: &LazyHash<Library>, source: &Source) -> SourceResult<typst::foundations::Module> {
    use comemo::Track;
    let traced = typst::engine::Traced::default();
    let mut sink = typst::engine::Sink::new();
    let route = typst::engine::Route::default();
    typst_eval::eval(
        world.track(),
        library,
        traced.track(),
        sink.track_mut(),
        route.track(),
        source,
    )
}

/// Whether a set of features is one the shims can be installed against.
///
/// The rules call `html.elem`, which is an error outside HTML export, so they
/// must only be installed when the HTML feature is enabled.
pub fn suits(features: &Features) -> bool {
    features.is_enabled(Feature::Html)
}
