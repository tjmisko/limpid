# Layout fixture

A paragraph with **bold**, *italic*, ***both***, `inline code`, ~~struck text~~
and a [link](https://example.com). It is long enough to wrap across several
lines so that line breaking, shaping and the content column are all exercised
by the snapshot.

## Code

```rust
// a comment
fn main() {
    let answer: u32 = 42;
    println!("hello, {answer}");
}
```

```
plain fence without a language and a very long line that does not wrap but scrolls: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
```

### Lists

- first item
- second item with `code`
  - nested one
  - nested two
    - deeper
- third item

1. ordered one
2. ordered two
3. ordered three

- [x] finished task
- [ ] open task

#### Quotes

> A plain block quote that is muted and tinted
> across two lines.

> [!NOTE]
> A note callout.

> [!TIP]
> A tip callout.

> [!IMPORTANT]
> An important callout.

> [!WARNING]
> A warning callout.

> [!CAUTION]
> A caution callout.

---

Closing paragraph after the rule.
