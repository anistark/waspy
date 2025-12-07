use crate::stdlib::StdlibValue;

pub fn get_attribute(_attr: &str) -> Option<StdlibValue> {
    None
}

pub fn get_function(func: &str) -> Option<CollectionsFunction> {
    match func {
        "namedtuple" => Some(CollectionsFunction::Namedtuple),
        "deque" => Some(CollectionsFunction::Deque),
        "Counter" => Some(CollectionsFunction::Counter),
        "OrderedDict" => Some(CollectionsFunction::OrderedDict),
        "defaultdict" => Some(CollectionsFunction::Defaultdict),
        "ChainMap" => Some(CollectionsFunction::ChainMap),
        "UserDict" => Some(CollectionsFunction::UserDict),
        "UserList" => Some(CollectionsFunction::UserList),
        "UserString" => Some(CollectionsFunction::UserString),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum CollectionsFunction {
    Namedtuple,
    Deque,
    Counter,
    OrderedDict,
    Defaultdict,
    ChainMap,
    UserDict,
    UserList,
    UserString,
}
