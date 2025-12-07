use crate::stdlib::StdlibValue;

pub fn get_attribute(_attr: &str) -> Option<StdlibValue> {
    None
}

pub fn get_function(func: &str) -> Option<RandomFunction> {
    match func {
        "random" => Some(RandomFunction::Random),
        "randint" => Some(RandomFunction::Randint),
        "randrange" => Some(RandomFunction::Randrange),
        "uniform" => Some(RandomFunction::Uniform),
        "choice" => Some(RandomFunction::Choice),
        "shuffle" => Some(RandomFunction::Shuffle),
        "sample" => Some(RandomFunction::Sample),
        "seed" => Some(RandomFunction::Seed),
        "getrandbits" => Some(RandomFunction::Getrandbits),
        "gauss" => Some(RandomFunction::Gauss),
        "normalvariate" => Some(RandomFunction::Normalvariate),
        "expovariate" => Some(RandomFunction::Expovariate),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum RandomFunction {
    Random,
    Randint,
    Randrange,
    Uniform,
    Choice,
    Shuffle,
    Sample,
    Seed,
    Getrandbits,
    Gauss,
    Normalvariate,
    Expovariate,
}
