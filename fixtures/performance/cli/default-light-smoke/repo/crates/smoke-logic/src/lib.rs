pub fn calculate() -> i32 {
    smoke_support::base() + 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn initial_calculation_is_two() {
        assert_eq!(super::calculate(), 2);
    }
}
