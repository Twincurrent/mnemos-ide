//! Mnemos IDE: "Copy as Markdown" for terminal blocks. It replaces Warp's "Share block", which
//! uploaded the block to Warp's servers; this stays local and only writes to the clipboard.

/// Formats one block as Markdown: the command in a `sh` fence, then its output (if any) in a
/// plain fence. Each fence is longer than the longest backtick run in its content, so output that
/// itself contains a fence cannot close it early.
pub fn block_as_markdown(command: &str, output: &str) -> String {
    let command = command.trim_end();
    let output = output.trim_end();
    let mut markdown = String::new();
    if !command.trim().is_empty() {
        push_fenced(&mut markdown, "sh", command);
    }
    if !output.trim().is_empty() {
        if !markdown.is_empty() {
            markdown.push('\n');
        }
        push_fenced(&mut markdown, "", output);
    }
    markdown
}

/// Joins several blocks' Markdown with a blank line between them, skipping empty blocks.
pub fn blocks_as_markdown<'a>(blocks: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    blocks
        .into_iter()
        .map(|(command, output)| block_as_markdown(command, output))
        .filter(|markdown| !markdown.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn push_fenced(markdown: &mut String, info: &str, content: &str) {
    let fence = "`".repeat(longest_backtick_run(content).max(2) + 1);
    markdown.push_str(&fence);
    markdown.push_str(info);
    markdown.push('\n');
    markdown.push_str(content);
    markdown.push('\n');
    markdown.push_str(&fence);
    markdown.push('\n');
}

fn longest_backtick_run(content: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for c in content.chars() {
        if c == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_and_output_are_fenced_separately() {
        assert_eq!(
            block_as_markdown("ls -la\n", "a.txt\nb.txt\n\n"),
            "```sh\nls -la\n```\n\n```\na.txt\nb.txt\n```\n"
        );
    }

    #[test]
    fn empty_output_gives_only_the_command() {
        assert_eq!(block_as_markdown("true", "  \n"), "```sh\ntrue\n```\n");
    }

    #[test]
    fn empty_command_gives_only_the_output() {
        assert_eq!(block_as_markdown("", "hello"), "```\nhello\n```\n");
        assert_eq!(block_as_markdown(" ", ""), "");
    }

    #[test]
    fn fence_outgrows_backticks_in_the_content() {
        assert_eq!(
            block_as_markdown("cat README.md", "```rust\nfn main() {}\n```"),
            "```sh\ncat README.md\n```\n\n````\n```rust\nfn main() {}\n```\n````\n"
        );
    }

    #[test]
    fn several_blocks_are_separated_by_a_blank_line() {
        assert_eq!(
            blocks_as_markdown([("echo a", "a"), ("", ""), ("echo b", "b")]),
            "```sh\necho a\n```\n\n```\na\n```\n\n```sh\necho b\n```\n\n```\nb\n```\n"
        );
    }
}
