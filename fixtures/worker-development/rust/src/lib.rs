pub fn twice(value: u32) -> u32 {
    value * 2
}

#[cfg(test)]
mod tests {
    #[test]
    fn twice_seven_is_fourteen() {
        assert_eq!(super::twice(7), 14);
    }
}
