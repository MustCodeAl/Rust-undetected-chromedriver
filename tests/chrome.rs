#[cfg(test)]
mod tests {
    use undetected_chromedriver::chrome;

    #[tokio::test]
    async fn test_chrome() {
        let driver = chrome().await.expect("Failed to create Chrome driver");
        assert!(driver.title().await.is_ok());
        driver.quit().await.expect("Failed to quit Chrome driver");
    }
}
