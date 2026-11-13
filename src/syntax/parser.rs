// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Recursive-descent parser with precedence climbing for expressions.
//!
//! Statements end at a newline, a `;`, a `}` or end of file. Newlines are
//! ignored inside parentheses, brackets and map braces, and directly after
//! a binary operator or a comma, so long expressions can wrap.

use super::ast::*;
use super::lexer::{tokenize, Lexer};
use super::span::Span;
use super::token::{StrPart, Token, TokenKind};
use crate::diagnostics::{syntax, Diagnostic};
use std::rc::Rc;

pub fn parse(src: &str) -> Result<Program, Diagnostic> {
    let tokens = tokenize(src)?;
    Parser::with_source(tokens, src).program()
}

/// Parse a single expression (used by `cig eval` and string interpolation).
pub fn parse_expr_src(src: &str, origin: Span) -> Result<Expr, Diagnostic> {
    let tokens = Lexer::with_origin(src, origin).run()?;
    let mut p = Parser::new(tokens);
    p.skip_newlines();
    let expr = p.expr()?;
    p.skip_newlines();
    if p.peek() != &TokenKind::Eof {
        return Err(syntax(
            format!("unexpected {} after expression", p.peek().describe()),
            p.span(),
        ));
    }
    Ok(expr)
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// The source, for slicing a compensation's text into the AST.
    src: Option<std::rc::Rc<str>>,
    /// Current nesting of brackets, parentheses and blocks, held under
    /// [`MAX_NESTING`]: past it the parser answers with a diagnostic instead
    /// of running out of stack.
    depth: usize,
}

/// Deeper than this and the parser refuses (E208). Real scripts nest tens of
/// levels; this is a ceiling, not a target.
pub const MAX_NESTING: usize = 5_000;

/// Operators chained in one expression (`a + b + c + ...`) build a tree as
/// deep as the chain is long; past this the parser refuses (E208) instead
/// of the checker or the evaluator running out of stack (a debug build
/// holds 12,000; a release build far more).
pub const MAX_CHAIN: usize = 10_000;

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            src: None,
            depth: 0,
        }
    }

    pub fn with_source(tokens: Vec<Token>, src: &str) -> Self {
        Self {
            tokens,
            pos: 0,
            src: Some(std::rc::Rc::from(src)),
            depth: 0,
        }
    }

    fn slice(&self, span: Span) -> Option<String> {
        self.src
            .as_ref()
            .and_then(|s| s.get(span.start..span.end).map(str::to_string))
    }

    // ----- token helpers -------------------------------------------------

    fn peek_at(&self, ahead: usize) -> &TokenKind {
        let idx = (self.pos + ahead).min(self.tokens.len() - 1);
        &self.tokens[idx].kind
    }

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.peek() == kind
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Result<Token, Diagnostic> {
        if self.at(&kind) {
            Ok(self.advance())
        } else {
            Err(syntax(
                format!("expected {what}, found {}", self.peek().describe()),
                self.span(),
            ))
        }
    }

    fn skip_newlines(&mut self) {
        while self.at(&TokenKind::Newline) {
            self.advance();
        }
    }

    fn ident(&mut self, what: &str) -> Result<(String, Span), Diagnostic> {
        match self.peek().clone() {
            TokenKind::Ident(name) => {
                let t = self.advance();
                Ok((name, t.span))
            }
            other => Err(syntax(
                format!("expected {what}, found {}", other.describe()),
                self.span(),
            )),
        }
    }

    /// A member name after `.`: an identifier, or a keyword used as a name.
    fn member_name(&mut self) -> Result<(String, Span), Diagnostic> {
        if let Some(word) = self.peek().keyword_text() {
            let t = self.advance();
            return Ok((word.to_string(), t.span));
        }
        self.ident("a member name after `.`")
    }

    /// A statement must be followed by a terminator.
    fn end_stmt(&mut self) -> Result<(), Diagnostic> {
        match self.peek() {
            TokenKind::Newline | TokenKind::Semicolon => {
                self.advance();
                self.skip_newlines();
                Ok(())
            }
            TokenKind::RBrace | TokenKind::Eof => Ok(()),
            other => Err(syntax(
                format!("expected end of statement, found {}", other.describe()),
                self.span(),
            )
            .code("E203")
            .with_hint("put each statement on its own line, or separate them with `;`")),
        }
    }

    /// `exhael x`: a lone identifier followed by more tokens on the same line
    /// is almost always a misspelled keyword.
    fn misspelled_keyword(&self, expr: &Expr) -> Option<Diagnostic> {
        let ExprKind::Ident(word) = &expr.kind else {
            return None;
        };
        if self.stmt_ended()
            || matches!(
                self.peek(),
                TokenKind::Assign
                    | TokenKind::PlusAssign
                    | TokenKind::MinusAssign
                    | TokenKind::StarAssign
                    | TokenKind::SlashAssign
            )
        {
            return None;
        }
        const KEYWORDS: &[&str] = &[
            "roll", "stick", "pull", "chain", "snuff", "exhale", "cough", "try", "burn", "if",
            "else", "while", "for", "break", "continue",
        ];
        let close = crate::interp::closest(word, KEYWORDS.iter().copied())?;
        Some(
            syntax(format!("`{word}` is not a statement"), expr.span)
                .code("E204")
                .with_hint(format!("did you mean `{close}`?")),
        )
    }

    // ----- program and statements ------------------------------------------

    pub fn program(&mut self) -> Result<Program, Diagnostic> {
        let mut body = Vec::new();
        self.skip_separators();
        while !self.at(&TokenKind::Eof) {
            if self.at(&TokenKind::RBrace) {
                return Err(syntax("unexpected `}` with no open block", self.span()));
            }
            body.push(self.stmt()?);
            self.skip_separators();
        }
        Ok(Program { body })
    }

    /// Between statements: newlines, and the `;` that may follow any
    /// statement, a block statement included (`if c { }; x`).
    fn skip_separators(&mut self) {
        while matches!(self.peek(), TokenKind::Newline | TokenKind::Semicolon) {
            self.advance();
        }
    }

    fn enter(&mut self) -> Result<(), Diagnostic> {
        if self.depth >= MAX_NESTING {
            return Err(syntax(
                format!("nesting deeper than {MAX_NESTING} levels"),
                self.span(),
            )
            .code("E208")
            .with_hint("flatten it; a value this deep is better built in a loop"));
        }
        self.depth += 1;
        Ok(())
    }

    fn block(&mut self) -> Result<Block, Diagnostic> {
        self.enter()?;
        let r = self.block_inner();
        self.depth -= 1;
        r
    }

    fn block_inner(&mut self) -> Result<Block, Diagnostic> {
        let open = self.expect(TokenKind::LBrace, "`{`")?;
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(syntax("this `{` is never closed", open.span)
                    .code("E202")
                    .with_hint("add a matching `}`"));
            }
            stmts.push(self.stmt()?);
            self.skip_separators();
        }
        let close = self.advance();
        Ok(Block {
            stmts,
            span: open.span.to(close.span),
        })
    }

    fn stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.span();
        match self.peek().clone() {
            TokenKind::Roll | TokenKind::Stick => {
                let mutable = matches!(self.advance().kind, TokenKind::Roll);
                let (name, _) = self.ident("a name")?;
                if !self.eat(&TokenKind::Assign) {
                    return Err(syntax(
                        format!(
                            "expected `=` after `{name}`, found {}",
                            self.peek().describe()
                        ),
                        self.span(),
                    )
                    .with_hint(if mutable {
                        "roll x = value"
                    } else {
                        "stick x = value"
                    }));
                }
                self.skip_newlines();
                let value = self.expr()?;
                self.end_stmt()?;
                Ok(Stmt::Declare {
                    name,
                    mutable,
                    value,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Pull => {
                self.advance();
                let (name, _) = self.ident("a function name after `pull`")?;
                let params = self.params()?;
                let body = self.block()?;
                Ok(Stmt::Pull(FnDecl {
                    name,
                    params,
                    body: Rc::new(body),
                    span: start.to(self.prev_span()),
                }))
            }
            TokenKind::Chain => {
                self.advance();
                let (name, _) = self.ident("a chain name after `chain`")?;
                let open = self.expect(TokenKind::LBrace, "`{`")?;
                let mut steps = Vec::new();
                self.skip_newlines();
                while !self.at(&TokenKind::RBrace) {
                    if self.at(&TokenKind::Eof) {
                        return Err(syntax("this `{` is never closed", open.span)
                            .code("E202")
                            .with_hint("add a matching `}`"));
                    }
                    steps.push(self.expr()?);
                    match self.peek() {
                        TokenKind::Comma | TokenKind::Newline | TokenKind::Semicolon => {
                            self.advance();
                            self.skip_newlines();
                        }
                        TokenKind::RBrace => {}
                        other => {
                            return Err(syntax(
                                format!(
                                    "expected a comma or a new line between chain steps, found {}",
                                    other.describe()
                                ),
                                self.span(),
                            ))
                        }
                    }
                }
                self.advance();
                if steps.is_empty() {
                    return Err(syntax(
                        format!("chain `{name}` has no steps"),
                        start.to(self.prev_span()),
                    )
                    .code("E207")
                    .with_hint(
                        "list the sticks to light, one per line: chain name { fetch, build }",
                    ));
                }
                Ok(Stmt::Chain(ChainDecl {
                    name,
                    steps,
                    span: start.to(self.prev_span()),
                }))
            }
            TokenKind::Snuff => {
                self.advance();
                let value = if self.stmt_ended() {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.end_stmt()?;
                Ok(Stmt::Snuff {
                    value,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Exhale => {
                self.advance();
                let mut values = Vec::new();
                if !self.stmt_ended() {
                    values.push(self.expr()?);
                    while self.eat(&TokenKind::Comma) {
                        self.skip_newlines();
                        values.push(self.expr()?);
                    }
                }
                self.end_stmt()?;
                Ok(Stmt::Exhale {
                    values,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Cough => {
                self.advance();
                if self.stmt_ended() {
                    return Err(syntax("`cough` needs a value to raise", start)
                        .with_hint("cough \"what went wrong\""));
                }
                let value = self.expr()?;
                self.end_stmt()?;
                Ok(Stmt::Cough {
                    value,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::If => self.if_stmt(),
            TokenKind::While => {
                self.advance();
                let cond = self.expr()?;
                let body = self.block()?;
                Ok(Stmt::While {
                    cond,
                    body,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::For => {
                self.advance();
                let (var, _) = self.ident("a loop variable after `for`")?;
                self.expect(TokenKind::In, "`in`")?;
                let iter = self.expr()?;
                let body = self.block()?;
                Ok(Stmt::For {
                    var,
                    iter,
                    body,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Break => {
                self.advance();
                self.end_stmt()?;
                Ok(Stmt::Break(start))
            }
            TokenKind::Continue => {
                self.advance();
                self.end_stmt()?;
                Ok(Stmt::Continue(start))
            }
            TokenKind::Try => {
                self.advance();
                let body = self.block()?;
                self.skip_newlines();
                let mut catch_var = None;
                let mut handler = None;
                if self.eat(&TokenKind::Ashtray) {
                    if let TokenKind::Ident(name) = self.peek().clone() {
                        self.advance();
                        catch_var = Some(name);
                    }
                    handler = Some(self.block()?);
                    self.skip_newlines();
                }
                let finally = if self.eat(&TokenKind::Finally) {
                    Some(self.block()?)
                } else {
                    None
                };
                if handler.is_none() && finally.is_none() {
                    return Err(syntax(
                        format!(
                            "expected `ashtray` or `finally` after the `try` block, found {}",
                            self.peek().describe()
                        ),
                        self.span(),
                    )
                    .with_hint("try { ... } ashtray err { ... }, or try { ... } finally { ... }"));
                }
                Ok(Stmt::Try {
                    body,
                    catch_var,
                    handler,
                    finally,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Pack if matches!(self.peek_at(1), TokenKind::LBrace) => {
                self.advance();
                let open = self.expect(TokenKind::LBrace, "`{`")?;
                let mut roots = Vec::new();
                let mut options = Vec::new();
                self.skip_newlines();
                while !self.at(&TokenKind::RBrace) {
                    if self.at(&TokenKind::Eof) {
                        return Err(syntax("this `{` is never closed", open.span)
                            .code("E202")
                            .with_hint("add a matching `}`"));
                    }
                    // `name: value` is an option; no root starts that way.
                    match (self.peek().clone(), self.peek_at(1)) {
                        (TokenKind::Ident(name), TokenKind::Colon) => {
                            let name_span = self.advance().span;
                            self.advance();
                            self.skip_newlines();
                            let value = self.expr()?;
                            options.push(PackOption {
                                name,
                                name_span,
                                value,
                            });
                        }
                        _ => roots.push(self.expr()?),
                    }
                    match self.peek() {
                        TokenKind::Comma | TokenKind::Newline | TokenKind::Semicolon => {
                            self.advance();
                            self.skip_newlines();
                        }
                        TokenKind::RBrace => {}
                        other => {
                            return Err(syntax(
                                format!(
                                    "expected a comma or a new line between pack roots, found {}",
                                    other.describe()
                                ),
                                self.span(),
                            ))
                        }
                    }
                }
                self.advance();
                if roots.is_empty() && options.is_empty() {
                    return Err(syntax("a pack declares at least one root", start)
                        .with_hint("pack { \"./build\" }"));
                }
                Ok(Stmt::Pack(PackDecl {
                    roots,
                    options,
                    span: start.to(self.prev_span()),
                }))
            }
            TokenKind::Burn => {
                self.advance();
                let class = if self.eat(&TokenKind::Unlit) {
                    BurnClass::Unlit
                } else {
                    BurnClass::Normal
                };
                // `burn (state) { }`: a named state map for the compensation;
                // `burn (state, label) { }` or `burn (label) { }`: a name for
                // the burn, any expression that is a string. A bare identifier
                // is the state map, as it always was.
                let (state, label) = if self.eat(&TokenKind::LParen) {
                    let bare_ident = matches!(self.peek(), TokenKind::Ident(_))
                        && matches!(self.peek_at(1), TokenKind::RParen | TokenKind::Comma);
                    let (state, label) = if bare_ident {
                        let (name, _) = self.ident("a state name inside `burn (...)`")?;
                        let label = if self.eat(&TokenKind::Comma) {
                            Some(self.expr()?)
                        } else {
                            None
                        };
                        (Some(name), label)
                    } else {
                        (None, Some(self.expr()?))
                    };
                    self.expect(TokenKind::RParen, "`)`")?;
                    (state, label)
                } else {
                    (None, None)
                };
                if !self.at(&TokenKind::LBrace) {
                    return Err(syntax(
                        format!(
                            "expected `{{` after `burn`, found {}",
                            self.peek().describe()
                        ),
                        self.span(),
                    )
                    .with_hint("burn { ... } or burn unlit { ... }"));
                }
                let body = self.block()?;
                self.skip_newlines();
                let (unburn, unburn_src) = if self.at(&TokenKind::Ident("unburn".to_string())) {
                    self.advance();
                    let open = self.span();
                    let block = self.block()?;
                    let close = self.prev_span();
                    let inner = Span {
                        start: open.end,
                        end: close.start.max(open.end),
                        line: open.line,
                        col: open.col,
                    };
                    (Some(block), self.slice(inner))
                } else {
                    (None, None)
                };
                Ok(Stmt::Burn {
                    class,
                    state,
                    label,
                    body,
                    unburn,
                    unburn_src,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Else => Err(syntax("`else` without a preceding `if`", start)),
            TokenKind::Ashtray => Err(syntax("`ashtray` without a preceding `try`", start)),
            TokenKind::Finally => Err(syntax("`finally` without a preceding `try`", start)
                .with_hint("try { ... } finally { ... }")),
            TokenKind::LBrace => Err(syntax("a bare `{` cannot start a statement", start)
                .with_hint(
                    "write `burn { ... }`; a map literal on its own line is not a statement",
                )),
            _ => self.expr_or_assign_stmt(),
        }
    }

    fn stmt_ended(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::RBrace | TokenKind::Eof
        )
    }

    fn if_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.span();
        self.expect(TokenKind::If, "`if`")?;
        let mut branches = Vec::new();
        let cond = self.expr()?;
        let body = self.block()?;
        branches.push((cond, body));
        let mut otherwise = None;
        loop {
            // `else` may sit on the next line.
            let save = self.pos;
            self.skip_newlines();
            if !self.eat(&TokenKind::Else) {
                self.pos = save;
                break;
            }
            if self.eat(&TokenKind::If) {
                let cond = self.expr()?;
                let body = self.block()?;
                branches.push((cond, body));
            } else {
                otherwise = Some(self.block()?);
                break;
            }
        }
        Ok(Stmt::If {
            branches,
            otherwise,
            span: start.to(self.prev_span()),
        })
    }

    fn expr_or_assign_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.span();
        let expr = self.expr()?;
        let op = match self.peek() {
            TokenKind::Assign => Some(AssignOp::Set),
            TokenKind::PlusAssign => Some(AssignOp::Add),
            TokenKind::MinusAssign => Some(AssignOp::Sub),
            TokenKind::StarAssign => Some(AssignOp::Mul),
            TokenKind::SlashAssign => Some(AssignOp::Div),
            _ => None,
        };
        if op.is_none() {
            if let Some(d) = self.misspelled_keyword(&expr) {
                return Err(d);
            }
        }
        if let Some(op) = op {
            let op_span = self.span();
            self.advance();
            let target = match expr.kind {
                ExprKind::Ident(name) => AssignTarget::Var(name, Default::default()),
                ExprKind::Index { object, index } => AssignTarget::Index {
                    object: *object,
                    index: *index,
                },
                ExprKind::Member {
                    object,
                    name,
                    safe: false,
                    hint,
                } => AssignTarget::Member {
                    object: *object,
                    name,
                    hint,
                },
                _ => {
                    return Err(
                        syntax("this is not something that can be assigned to", expr.span)
                            .code("E205")
                            .with_hint("assign to a name, `list[i]` or `map.key`"),
                    )
                }
            };
            self.skip_newlines();
            let value = self.expr()?;
            self.end_stmt()?;
            let _ = op_span;
            return Ok(Stmt::Assign {
                target,
                op,
                value,
                span: start.to(self.prev_span()),
            });
        }
        self.end_stmt()?;
        Ok(Stmt::Expr(expr))
    }

    fn params(&mut self) -> Result<Vec<String>, Diagnostic> {
        self.expect(TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RParen) {
            let (name, span) = self.ident("a parameter name")?;
            if params.contains(&name) {
                return Err(syntax(format!("duplicate parameter `{name}`"), span));
            }
            params.push(name);
            self.skip_newlines();
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.skip_newlines();
        self.expect(TokenKind::RParen, "`)` to close the parameter list")?;
        Ok(params)
    }

    // ----- expressions -------------------------------------------------------

    pub fn expr(&mut self) -> Result<Expr, Diagnostic> {
        self.pipe()
    }

    fn pipe(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.or()?;
        while self.at(&TokenKind::Pipe) {
            self.advance();
            self.skip_newlines();
            let rhs = self.or()?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr::new(
                ExprKind::Pipe {
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }
        Ok(lhs)
    }

    fn binary_left<F>(&mut self, ops: &[(TokenKind, BinOp)], next: F) -> Result<Expr, Diagnostic>
    where
        F: Fn(&mut Self) -> Result<Expr, Diagnostic>,
    {
        let mut lhs = next(self)?;
        let mut chained = 0usize;
        'outer: loop {
            for (tok, op) in ops {
                if self.at(tok) {
                    chained += 1;
                    if chained > MAX_CHAIN {
                        return Err(syntax(
                            format!("more than {MAX_CHAIN} operators chained in one expression"),
                            self.span(),
                        )
                        .code("E208")
                        .with_hint("split it across statements, or build the value in a loop"));
                    }
                    self.advance();
                    self.skip_newlines();
                    let rhs = next(self)?;
                    let span = lhs.span.to(rhs.span);
                    lhs = Expr::new(
                        ExprKind::Binary {
                            op: *op,
                            lhs: Box::new(lhs),
                            rhs: Box::new(rhs),
                        },
                        span,
                    );
                    continue 'outer;
                }
            }
            break;
        }
        Ok(lhs)
    }

    fn or(&mut self) -> Result<Expr, Diagnostic> {
        self.binary_left(&[(TokenKind::Or, BinOp::Or)], Self::and)
    }

    fn and(&mut self) -> Result<Expr, Diagnostic> {
        self.binary_left(&[(TokenKind::And, BinOp::And)], Self::not)
    }

    fn not(&mut self) -> Result<Expr, Diagnostic> {
        if self.at(&TokenKind::Not) {
            let start = self.span();
            self.advance();
            let expr = self.not()?;
            let span = start.to(expr.span);
            return Ok(Expr::new(
                ExprKind::Unary {
                    op: UnOp::Not,
                    expr: Box::new(expr),
                },
                span,
            ));
        }
        self.coalesce()
    }

    fn coalesce(&mut self) -> Result<Expr, Diagnostic> {
        self.binary_left(&[(TokenKind::Coalesce, BinOp::Coalesce)], Self::comparison)
    }

    fn comparison(&mut self) -> Result<Expr, Diagnostic> {
        let lhs = self.range()?;
        let op = match self.peek() {
            TokenKind::EqEq => BinOp::Eq,
            TokenKind::NotEq => BinOp::Ne,
            TokenKind::Lt => BinOp::Lt,
            TokenKind::LtEq => BinOp::Le,
            TokenKind::Gt => BinOp::Gt,
            TokenKind::GtEq => BinOp::Ge,
            _ => return Ok(lhs),
        };
        self.advance();
        self.skip_newlines();
        let rhs = self.range()?;
        let span = lhs.span.to(rhs.span);
        let expr = Expr::new(
            ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            span,
        );
        if matches!(
            self.peek(),
            TokenKind::EqEq
                | TokenKind::NotEq
                | TokenKind::Lt
                | TokenKind::LtEq
                | TokenKind::Gt
                | TokenKind::GtEq
        ) {
            return Err(syntax("comparisons cannot be chained", self.span())
                .code("E206")
                .with_hint("write `a < b and b < c`"));
        }
        Ok(expr)
    }

    fn range(&mut self) -> Result<Expr, Diagnostic> {
        let lhs = self.additive()?;
        if self.at(&TokenKind::DotDot) {
            self.advance();
            self.skip_newlines();
            let rhs = self.additive()?;
            let span = lhs.span.to(rhs.span);
            return Ok(Expr::new(
                ExprKind::Binary {
                    op: BinOp::Range,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            ));
        }
        Ok(lhs)
    }

    fn additive(&mut self) -> Result<Expr, Diagnostic> {
        self.binary_left(
            &[
                (TokenKind::Plus, BinOp::Add),
                (TokenKind::Minus, BinOp::Sub),
            ],
            Self::multiplicative,
        )
    }

    fn multiplicative(&mut self) -> Result<Expr, Diagnostic> {
        self.binary_left(
            &[
                (TokenKind::Star, BinOp::Mul),
                (TokenKind::Slash, BinOp::Div),
                (TokenKind::Percent, BinOp::Rem),
            ],
            Self::unary,
        )
    }

    fn unary(&mut self) -> Result<Expr, Diagnostic> {
        if self.at(&TokenKind::Minus) {
            let start = self.span();
            self.advance();
            // `-9223372036854775808` is the one literal whose magnitude does
            // not fit before the sign is applied: fold the sign in here.
            if self.at(&TokenKind::IntMinMagnitude) {
                let end = self.span();
                self.advance();
                return Ok(Expr::new(ExprKind::Int(i64::MIN), start.to(end)));
            }
            let expr = self.unary()?;
            let span = start.to(expr.span);
            return Ok(Expr::new(
                ExprKind::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(expr),
                },
                span,
            ));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.primary()?;
        loop {
            match self.peek() {
                TokenKind::LParen => {
                    self.advance();
                    let args = self.args(TokenKind::RParen, "`)`")?;
                    let span = expr.span.to(self.prev_span());
                    expr = Expr::new(
                        ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                        span,
                    );
                }
                TokenKind::Dot | TokenKind::SafeDot => {
                    let safe = matches!(self.advance().kind, TokenKind::SafeDot);
                    let (name, _) = self.member_name()?;
                    if self.at(&TokenKind::LParen) {
                        self.advance();
                        let args = self.args(TokenKind::RParen, "`)`")?;
                        let span = expr.span.to(self.prev_span());
                        expr = Expr::new(
                            ExprKind::Method {
                                receiver: Box::new(expr),
                                name,
                                args,
                                safe,
                            },
                            span,
                        );
                    } else {
                        let span = expr.span.to(self.prev_span());
                        expr = Expr::new(
                            ExprKind::Member {
                                object: Box::new(expr),
                                name,
                                safe,
                                hint: Default::default(),
                            },
                            span,
                        );
                    }
                }
                TokenKind::LBracket => {
                    self.advance();
                    self.skip_newlines();
                    let index = self.expr()?;
                    self.skip_newlines();
                    self.expect(TokenKind::RBracket, "`]`")?;
                    let span = expr.span.to(self.prev_span());
                    expr = Expr::new(
                        ExprKind::Index {
                            object: Box::new(expr),
                            index: Box::new(index),
                        },
                        span,
                    );
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// Parse a comma-separated list up to `close` (already past the opener).
    fn args(&mut self, close: TokenKind, what: &str) -> Result<Vec<Expr>, Diagnostic> {
        let mut items = Vec::new();
        self.skip_newlines();
        while !self.at(&close) {
            if self.at(&TokenKind::Eof) {
                return Err(syntax(format!("expected {what}"), self.span()));
            }
            items.push(self.expr()?);
            self.skip_newlines();
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.skip_newlines();
        self.expect(close, what)?;
        Ok(items)
    }

    fn primary(&mut self) -> Result<Expr, Diagnostic> {
        self.enter()?;
        let r = self.primary_inner();
        self.depth -= 1;
        r
    }

    fn primary_inner(&mut self) -> Result<Expr, Diagnostic> {
        let tok = self.advance();
        let span = tok.span;
        let kind = match tok.kind {
            TokenKind::Null => ExprKind::Null,
            TokenKind::True => ExprKind::Bool(true),
            TokenKind::False => ExprKind::Bool(false),
            TokenKind::Int(v) => ExprKind::Int(v),
            TokenKind::IntMinMagnitude => {
                return Err(syntax(
                    "integer literal `9223372036854775808` does not fit in 64 bits".to_string(),
                    span,
                )
                .code("E104")
                .with_hint(
                    "the largest int is 9223372036854775807; -9223372036854775808 is valid with the minus sign directly in front",
                ))
            }
            TokenKind::Float(v) => ExprKind::Float(v),
            TokenKind::Str(parts) => ExprKind::Str(self.string_pieces(parts)?),
            TokenKind::Ident(name) => ExprKind::Ident(name),
            TokenKind::LParen => {
                self.skip_newlines();
                let inner = self.expr()?;
                self.skip_newlines();
                self.expect(TokenKind::RParen, "`)`")?;
                return Ok(Expr::new(inner.kind, span.to(self.prev_span())));
            }
            TokenKind::LBracket => {
                let items = self.args(TokenKind::RBracket, "`]`")?;
                ExprKind::List(items)
            }
            TokenKind::LBrace => ExprKind::Map(self.map_entries()?),
            TokenKind::Pack => {
                let params = self.params()?;
                let body = if self.eat(&TokenKind::FatArrow) {
                    self.skip_newlines();
                    LambdaBody::Expr(Rc::new(self.expr()?))
                } else if self.at(&TokenKind::LBrace) {
                    LambdaBody::Block(Rc::new(self.block()?))
                } else {
                    return Err(syntax(
                        format!(
                            "expected `=>` or `{{` after pack parameters, found {}",
                            self.peek().describe()
                        ),
                        self.span(),
                    )
                    .with_hint("pack(x) => x + 1   or   pack(x) { snuff x + 1 }"));
                };
                ExprKind::Lambda { params, body }
            }
            TokenKind::Newline | TokenKind::Eof => {
                return Err(syntax("expected an expression, found end of line", span).code("E201"))
            }
            other => {
                return Err(syntax(
                    format!("expected an expression, found {}", other.describe()),
                    span,
                )
                .code("E201"))
            }
        };
        Ok(Expr::new(kind, span.to(self.prev_span())))
    }

    fn map_entries(&mut self) -> Result<Vec<(MapKey, Expr)>, Diagnostic> {
        let mut entries = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) {
            let key = match self.peek().clone() {
                TokenKind::Ident(name) => {
                    self.advance();
                    MapKey::Static(name)
                }
                kw if kw.keyword_text().is_some()
                    && self.tokens[self.pos + 1].kind == TokenKind::Colon =>
                {
                    self.advance();
                    MapKey::Static(kw.keyword_text().unwrap_or_default().to_string())
                }
                TokenKind::Str(parts) => {
                    let t = self.advance();
                    let pieces = self.string_pieces(parts)?;
                    if pieces.iter().all(|p| matches!(p, StrPiece::Lit(_))) {
                        let text = pieces
                            .into_iter()
                            .map(|p| match p {
                                StrPiece::Lit(s) => s,
                                StrPiece::Expr(_) => unreachable!(),
                            })
                            .collect::<String>();
                        MapKey::Static(text)
                    } else {
                        MapKey::Dynamic(Expr::new(ExprKind::Str(pieces), t.span))
                    }
                }
                TokenKind::LBracket => {
                    self.advance();
                    self.skip_newlines();
                    let e = self.expr()?;
                    self.skip_newlines();
                    self.expect(TokenKind::RBracket, "`]`")?;
                    MapKey::Dynamic(e)
                }
                TokenKind::Int(v) => {
                    self.advance();
                    MapKey::Static(v.to_string())
                }
                other => {
                    return Err(syntax(
                        format!("expected a map key, found {}", other.describe()),
                        self.span(),
                    )
                    .with_hint(
                        "write the key as `name: value`, `\"text\": value` or `[expr]: value`",
                    ))
                }
            };
            self.expect(TokenKind::Colon, "`:` after the map key")?;
            self.skip_newlines();
            let value = self.expr()?;
            entries.push((key, value));
            self.skip_newlines();
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.skip_newlines();
        self.expect(TokenKind::RBrace, "`}` to close the map")?;
        Ok(entries)
    }

    fn string_pieces(&mut self, parts: Vec<StrPart>) -> Result<Vec<StrPiece>, Diagnostic> {
        let mut pieces = Vec::with_capacity(parts.len());
        for part in parts {
            pieces.push(match part {
                StrPart::Lit(s) => StrPiece::Lit(s),
                StrPart::Interp { src, span } => StrPiece::Expr(parse_expr_src(&src, span)?),
            });
        }
        Ok(pieces)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> Program {
        match parse(src) {
            Ok(p) => p,
            Err(e) => panic!("{}", e.render(None, Some(src))),
        }
    }

    fn err(src: &str) -> Diagnostic {
        parse(src).expect_err("expected a parse error")
    }

    #[test]
    fn parses_declarations_and_functions() {
        let p = ok(
            "roll a = 1\nstick b = a + 2\npull add(x, y) {\n  snuff x + y\n}\nexhale add(a, b)\n",
        );
        assert_eq!(p.body.len(), 4);
        assert!(matches!(p.body[0], Stmt::Declare { mutable: true, .. }));
        assert!(matches!(p.body[1], Stmt::Declare { mutable: false, .. }));
        assert!(matches!(p.body[2], Stmt::Pull(_)));
    }

    #[test]
    fn precedence_is_sane() {
        let p = ok("roll v = 1 + 2 * 3 == 7 and not false");
        let Stmt::Declare { value, .. } = &p.body[0] else {
            panic!()
        };
        // and( ==( +(1, *(2,3)), 7 ), not(false) )
        let ExprKind::Binary {
            op: BinOp::And,
            lhs,
            ..
        } = &value.kind
        else {
            panic!("top should be `and`, got {:?}", value.kind)
        };
        let ExprKind::Binary {
            op: BinOp::Eq,
            lhs: sum,
            ..
        } = &lhs.kind
        else {
            panic!("expected `==`")
        };
        assert!(matches!(sum.kind, ExprKind::Binary { op: BinOp::Add, .. }));
    }

    #[test]
    fn pipe_is_lowest() {
        let p = ok("roll v = a + 1 >> f(2) >> g");
        let Stmt::Declare { value, .. } = &p.body[0] else {
            panic!()
        };
        let ExprKind::Pipe { rhs, lhs } = &value.kind else {
            panic!()
        };
        assert!(matches!(rhs.kind, ExprKind::Ident(_)));
        assert!(matches!(lhs.kind, ExprKind::Pipe { .. }));
    }

    #[test]
    fn statements_need_terminators() {
        let e = err("roll a = 1 roll b = 2");
        assert!(e.message.contains("end of statement"));
        ok("roll a = 1; roll b = 2");
    }

    #[test]
    fn multiline_calls_lists_and_maps() {
        ok("roll m = {\n  name: \"x\",\n  \"with space\": [1,\n 2,\n 3],\n  [1 + 1]: null,\n}\nroll r = f(\n  1,\n  2,\n)\n");
    }

    #[test]
    fn if_else_chains_and_else_on_next_line() {
        let p = ok("if a {\n exhale 1\n} else if b {\n exhale 2\n}\nelse {\n exhale 3\n}\n");
        let Stmt::If {
            branches,
            otherwise,
            ..
        } = &p.body[0]
        else {
            panic!()
        };
        assert_eq!(branches.len(), 2);
        assert!(otherwise.is_some());
    }

    #[test]
    fn burn_try_and_lambdas() {
        let p = ok("burn unlit {\n fs.rm(\"x\")\n}\ntry {\n cough \"boom\"\n} ashtray e {\n exhale e.message\n}\nroll inc = pack(x) => x + 1\nroll blk = pack(x) { snuff x }\n");
        assert!(matches!(
            p.body[0],
            Stmt::Burn {
                class: BurnClass::Unlit,
                ..
            }
        ));
        assert!(
            matches!(&p.body[1], Stmt::Try { catch_var: Some(v), handler: Some(_), finally: None, .. } if v == "e")
        );
        let q = ok("try {\n exhale 1\n} finally {\n exhale 2\n}\ntry {\n exhale 1\n} ashtray {\n exhale 2\n} finally {\n exhale 3\n}\n");
        assert!(matches!(
            &q.body[0],
            Stmt::Try {
                handler: None,
                finally: Some(_),
                ..
            }
        ));
        assert!(matches!(
            &q.body[1],
            Stmt::Try {
                handler: Some(_),
                finally: Some(_),
                catch_var: None,
                ..
            }
        ));
    }

    #[test]
    fn assignment_targets() {
        ok("a = 1\na += 2\nm.k = 3\nl[0] = 4\n");
        let e = err("f() = 1");
        assert!(e.message.contains("assigned"));
    }

    #[test]
    fn helpful_errors() {
        assert!(err("roll = 1").message.contains("expected a name"));
        assert!(err("if x {\n exhale 1\n").message.contains("never closed"));
        assert!(err("a < b < c").hint.is_some());
        assert!(err("else {}").message.contains("`else`"));
    }

    #[test]
    fn interpolation_parses_expressions() {
        let p = ok("exhale \"n=${n + 1} ok\"");
        let Stmt::Exhale { values, .. } = &p.body[0] else {
            panic!()
        };
        let ExprKind::Str(pieces) = &values[0].kind else {
            panic!()
        };
        assert!(matches!(pieces[1], StrPiece::Expr(_)));
    }

    #[test]
    fn a_pack_takes_options_beside_its_roots() {
        let p = ok("pack { \"./a\", own_flags: true, \"./b\" }\n");
        let Stmt::Pack(d) = &p.body[0] else { panic!() };
        assert_eq!(d.roots.len(), 2);
        assert_eq!(d.options.len(), 1);
        assert_eq!(d.options[0].name, "own_flags");
        assert_eq!(
            (d.options[0].name_span.line, d.options[0].name_span.col),
            (1, 15)
        );
        assert!(p.own_flags());
        // No root: allowed, a pack that declares no scope.
        let p = ok("pack {\n  own_flags: true\n}\n");
        let Stmt::Pack(d) = &p.body[0] else { panic!() };
        assert!(d.roots.is_empty() && p.own_flags());
        assert!(!ok("pack { own_flags: false }\n").own_flags());
        assert!(!ok("pack { \".\" }\n").own_flags());
        assert!(!ok("exhale 1\n").own_flags());
        // Only the literal `true` says so; anything else is the checker's.
        assert!(!ok("pack { own_flags: \"true\" }\n").own_flags());
        // Neither root nor option is still a syntax error.
        assert!(err("pack { }\n").message.contains("at least one root"));
    }
}
