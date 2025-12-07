use crate::stdlib::StdlibValue;

pub fn get_attribute(_attr: &str) -> Option<StdlibValue> {
    None
}

pub fn get_function(func: &str) -> Option<FunctoolsFunction> {
    match func {
        "reduce" => Some(FunctoolsFunction::Reduce),
        "partial" => Some(FunctoolsFunction::Partial),
        "partialmethod" => Some(FunctoolsFunction::Partialmethod),
        "wraps" => Some(FunctoolsFunction::Wraps),
        "update_wrapper" => Some(FunctoolsFunction::UpdateWrapper),
        "total_ordering" => Some(FunctoolsFunction::TotalOrdering),
        "cmp_to_key" => Some(FunctoolsFunction::CmpToKey),
        "lru_cache" => Some(FunctoolsFunction::LruCache),
        "cache" => Some(FunctoolsFunction::Cache),
        "cached_property" => Some(FunctoolsFunction::CachedProperty),
        "singledispatch" => Some(FunctoolsFunction::Singledispatch),
        "singledispatchmethod" => Some(FunctoolsFunction::Singledispatchmethod),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum FunctoolsFunction {
    Reduce,
    Partial,
    Partialmethod,
    Wraps,
    UpdateWrapper,
    TotalOrdering,
    CmpToKey,
    LruCache,
    Cache,
    CachedProperty,
    Singledispatch,
    Singledispatchmethod,
}
