pub fn base() -> i32 {
    2
}

#[cfg(test)]
mod tests {
    #[test]
    fn repaired_base_is_two() {
        assert_eq!(super::base(), 2);
    }
}
