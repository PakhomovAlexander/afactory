pub fn value() -> i32 {
    smoke_logic::calculate()
}

pub fn feature() -> i32 {
    smoke_logic::calculate() + 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn feature_extends_the_current_calculation() {
        assert_eq!(super::feature(), smoke_logic::calculate() + 1);
    }
}
