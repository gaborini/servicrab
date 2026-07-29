```markdown
# servicrab Development Patterns

> Auto-generated skill from repository analysis

## Overview
This skill teaches the core development conventions and workflows used in the `servicrab` Rust codebase. You'll learn about file naming, import/export styles, commit patterns, and how to structure and run tests. This guide is ideal for contributors aiming for consistency and best practices in `servicrab`.

## Coding Conventions

### File Naming
- Use **camelCase** for file names.
  - Example: `myModule.rs`, `userService.rs`

### Import Style
- Prefer **relative imports** within the codebase.
  - Example:
    ```rust
    mod utils;
    use crate::utils::helperFunction;
    ```

### Export Style
- Use **named exports** for modules and functions.
  - Example:
    ```rust
    pub fn processRequest() { /* ... */ }
    ```

### Commit Patterns
- Commit types are **mixed**, with a preference for the `feat` prefix for new features.
- Commit messages are concise, averaging 29 characters.
  - Example: `feat: add user authentication`

## Workflows

### Adding a New Feature
**Trigger:** When implementing a new functionality  
**Command:** `/add-feature`

1. Create a new file using camelCase naming.
2. Implement the feature using relative imports as needed.
3. Export public functions or structs with `pub`.
4. Write corresponding tests in a `*.test.*` file.
5. Commit changes with a message starting with `feat:`.
6. Open a pull request for review.

### Refactoring Existing Code
**Trigger:** When improving or restructuring code without changing its behavior  
**Command:** `/refactor`

1. Identify the target module or function.
2. Refactor code, maintaining camelCase file naming and relative imports.
3. Ensure all exports remain named and public interfaces are preserved.
4. Update or add tests if necessary.
5. Commit with a descriptive message (e.g., `refactor: optimize user lookup`).

### Writing and Running Tests
**Trigger:** When verifying code correctness  
**Command:** `/test`

1. Create or update test files following the `*.test.*` pattern (e.g., `userService.test.rs`).
2. Write tests using the Rust testing framework (e.g., `#[cfg(test)]` and `#[test]`).
   ```rust
   #[cfg(test)]
   mod tests {
       use super::*;

       #[test]
       fn test_process_request() {
           // test logic here
       }
   }
   ```
3. Run tests using Cargo:
   ```
   cargo test
   ```
4. Review test results and fix any failures.

## Testing Patterns

- Test files follow the `*.test.*` naming convention (e.g., `orderService.test.rs`).
- Tests are written using Rust's built-in testing framework:
  - Annotate test modules with `#[cfg(test)]`.
  - Annotate test functions with `#[test]`.
- Example:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn test_example() {
          assert_eq!(2 + 2, 4);
      }
  }
  ```

## Commands
| Command        | Purpose                                      |
|----------------|----------------------------------------------|
| /add-feature   | Start the workflow for adding a new feature  |
| /refactor      | Begin a code refactoring workflow            |
| /test          | Run or write tests for the codebase          |
```