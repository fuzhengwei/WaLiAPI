export function formatToolArguments(argumentsValue: unknown): string {
  if (argumentsValue === undefined || argumentsValue === null || argumentsValue === "") return "{}";
  let formatted = typeof argumentsValue === "string" ? argumentsValue : JSON.stringify(argumentsValue, null, 2);
  if (typeof argumentsValue === "string") {
    try {
      const parsed: unknown = JSON.parse(argumentsValue);
      formatted = typeof parsed === "string" ? parsed : JSON.stringify(parsed, null, 2);
    } catch {
      formatted = argumentsValue;
    }
  }
  return formatted.replace(/"(?:\\.|[^"\\\r\n])*"|'(?:\\.|[^'\\\r\n])*'|`(?:\\.|[^`\\])*`/g, (literal) => {
    if (!literal.startsWith('"')) return literal;
    try {
      const decoded: string = JSON.parse(literal);
      return /[\r\n]/.test(decoded) ? `"${decoded.replace(/\r\n?/g, "\n")}"` : literal;
    } catch {
      return literal;
    }
  });
}
