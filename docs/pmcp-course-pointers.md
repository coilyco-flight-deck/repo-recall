# pmcp course pointers

Lookup table for "thing I need to do during the repo-recall MCP App rewrite" mapped to the course page that covers it.

Course root: <https://paiml.github.io/rust-mcp-sdk/course/>

| Task | Course page |
|------|-------------|
| Understand the overall MCP App architecture (widget + server protocol) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-mcp-apps.html> |
| Register widget HTML as a resource with the right MIME type | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-01-ui-resources.html> |
| Pick the right `UIResource` constructor (`html_mcp_app` vs legacy `html_mcp`) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-01-ui-resources.html> |
| Declare CSP for external image/API domains via `WidgetCSP` | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-01-ui-resources.html> |
| Embed widget HTML at compile time with `include_str!` | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-01-ui-resources.html> |
| Hot-reload widget HTML during dev | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-01-ui-resources.html> |
| Associate a tool with its widget (`ToolInfo::with_ui`) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-02-tool-ui-association.html> |
| Return `structuredContent` from a tool handler | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-02-tool-ui-association.html> |
| Add `outputSchema` to a tool | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-02-tool-ui-association.html> |
| Enable ChatGPT compatibility (`with_host_layer(HostType::ChatGpt)`) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-02-tool-ui-association.html> |
| Add `WidgetMeta` (border, description, CSP) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-02-tool-ui-association.html> |
| Build a widget with the ext-apps `App` class | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-03-postmessage.html> |
| Register all five required protocol handlers before `connect()` | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-03-postmessage.html> |
| Read initial data from `hostContext` vs subscribing via `ontoolresult` | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-03-postmessage.html> |
| Call back into the server from a widget (`app.callServerTool`) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-03-postmessage.html> |
| Build a React widget with `useApp` / `useHostStyles` | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-03-postmessage.html> |
| Bundle widgets with Vite + `vite-plugin-singlefile` | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-03-postmessage.html> |
| Load external images safely across hosts (fetch-to-blob) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-03-postmessage.html> |
| Diagnose "widget appears then connection dies" | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-03-postmessage.html> |
| Scaffold a new MCP App project (`cargo pmcp app new`) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-mcp-apps.html> |
| Preview a widget in the browser (`cargo pmcp preview`) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-mcp-apps.html> |
| Validate App metadata (`mcp-tester apps`, `cargo pmcp test apps`) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch20-mcp-apps.html> |
| Set up the `Server::builder()` and `ToolHandler` skeleton | <https://paiml.github.io/rust-mcp-sdk/course/part1-foundations/ch02-04-code-walkthrough.html> |
| Wire serde + JsonSchema input/output structs | <https://paiml.github.io/rust-mcp-sdk/course/part1-foundations/ch02-04-code-walkthrough.html> |
| Pattern-match `Error::validation` vs `Error::internal` vs `Error::not_found` | <https://paiml.github.io/rust-mcp-sdk/course/part1-foundations/ch02-04-code-walkthrough.html> |
| Use `tracing` and `#[instrument]` in handlers | <https://paiml.github.io/rust-mcp-sdk/course/part1-foundations/ch02-04-code-walkthrough.html> |
| Set up sqlx + sqlite connection pool (`Arc<Pool<Sqlite>>`) | <https://paiml.github.io/rust-mcp-sdk/course/part1-foundations/ch03-02-db-explorer.html> |
| Build `list_tables` / `query` style tools over sqlite | <https://paiml.github.io/rust-mcp-sdk/course/part1-foundations/ch03-02-db-explorer.html> |
| Read sqlite in read-only mode (`mode=ro`) | <https://paiml.github.io/rust-mcp-sdk/course/part1-foundations/ch03-03-sql-safety.html> |
| Apply parameterized queries, table allowlists, query timeouts | <https://paiml.github.io/rust-mcp-sdk/course/part1-foundations/ch03-03-sql-safety.html> |
| Audit-log query structure without leaking values | <https://paiml.github.io/rust-mcp-sdk/course/part1-foundations/ch03-03-sql-safety.html> |
| Decide tool vs resource for a given operation | <https://paiml.github.io/rust-mcp-sdk/course/part2-design/ch06-01-resources-vs-tools.html> |
| Add `outputSchema` for composition | <https://paiml.github.io/rust-mcp-sdk/course/part2-design/ch05-02-output-schemas.html> |
| Avoid the "50 confusing tools" anti-pattern | <https://paiml.github.io/rust-mcp-sdk/course/part2-design/ch04-01-antipatterns.html> |
| Pick a deployment target (Lambda vs Workers vs Cloud Run) | <https://paiml.github.io/rust-mcp-sdk/course/part3-deployment/ch07-01-options.html> |
| Build a multi-stage Rust Dockerfile with cargo-chef | <https://paiml.github.io/rust-mcp-sdk/course/part3-deployment/ch10-01-containers.html> |
| Configure auto-scaling for container-based MCP | <https://paiml.github.io/rust-mcp-sdk/course/part3-deployment/ch10-02-scaling.html> |
| Drive scenarios with `cargo pmcp test` and YAML | <https://paiml.github.io/rust-mcp-sdk/course/part4-testing/ch11-02-mcp-tester.html> |
| Run scheduled health checks via test scenarios | <https://paiml.github.io/rust-mcp-sdk/course/part7-observability/ch17-middleware.html> |
| Wire OAuth bearer tokens into MCP requests | <https://paiml.github.io/rust-mcp-sdk/course/part5-security/ch13-02-oauth-basics.html> |
| Pass user tokens through to backend systems | <https://paiml.github.io/rust-mcp-sdk/course/part5-security/ch13-03-validation.html> |
| Turn on built-in observability with one line | <https://paiml.github.io/rust-mcp-sdk/course/part7-observability/ch17-middleware.html> |
| Configure CloudWatch EMF or pretty console output | <https://paiml.github.io/rust-mcp-sdk/course/part7-observability/ch17-middleware.html> |
| Add custom protocol-layer middleware (priority enum) | <https://paiml.github.io/rust-mcp-sdk/course/part7-observability/ch17-01-architecture.html> |
| Structured logging best practices | <https://paiml.github.io/rust-mcp-sdk/course/part7-observability/ch17-02-logging.html> |
| Metrics collection patterns | <https://paiml.github.io/rust-mcp-sdk/course/part7-observability/ch17-03-metrics.html> |
| Add task support for long-running operations | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch21-tasks.html> |
| Configure a `TaskStore` (in-memory, DynamoDB, Redis) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch21-01-lifecycle.html> |
| Write a dual-path handler that branches on `is_task_request()` | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch21-01-lifecycle.html> |
| Per-request capability negotiation for tasks | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch21-02-capability-negotiation.html> |
| Validate LLM-generated SQL/GraphQL/JS before execution (Code Mode) | <https://paiml.github.io/rust-mcp-sdk/course/part8-advanced/ch22-code-mode.html> |
