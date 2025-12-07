use crate::stdlib::StdlibValue;

pub fn get_attribute(_attr: &str) -> Option<StdlibValue> {
    None
}

pub fn get_function(func: &str) -> Option<ItertoolsFunction> {
    match func {
        "count" => Some(ItertoolsFunction::Count),
        "cycle" => Some(ItertoolsFunction::Cycle),
        "repeat" => Some(ItertoolsFunction::Repeat),
        "chain" => Some(ItertoolsFunction::Chain),
        "compress" => Some(ItertoolsFunction::Compress),
        "dropwhile" => Some(ItertoolsFunction::Dropwhile),
        "filterfalse" => Some(ItertoolsFunction::Filterfalse),
        "groupby" => Some(ItertoolsFunction::Groupby),
        "islice" => Some(ItertoolsFunction::Islice),
        "starmap" => Some(ItertoolsFunction::Starmap),
        "takewhile" => Some(ItertoolsFunction::Takewhile),
        "tee" => Some(ItertoolsFunction::Tee),
        "zip_longest" => Some(ItertoolsFunction::ZipLongest),
        "product" => Some(ItertoolsFunction::Product),
        "permutations" => Some(ItertoolsFunction::Permutations),
        "combinations" => Some(ItertoolsFunction::Combinations),
        "combinations_with_replacement" => Some(ItertoolsFunction::CombinationsWithReplacement),
        "accumulate" => Some(ItertoolsFunction::Accumulate),
        "batched" => Some(ItertoolsFunction::Batched),
        "pairwise" => Some(ItertoolsFunction::Pairwise),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum ItertoolsFunction {
    Count,
    Cycle,
    Repeat,
    Chain,
    Compress,
    Dropwhile,
    Filterfalse,
    Groupby,
    Islice,
    Starmap,
    Takewhile,
    Tee,
    ZipLongest,
    Product,
    Permutations,
    Combinations,
    CombinationsWithReplacement,
    Accumulate,
    Batched,
    Pairwise,
}
