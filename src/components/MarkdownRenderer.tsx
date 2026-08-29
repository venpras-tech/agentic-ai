import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Renders Markdown with GFM support (tables, strikethrough, task lists).
 * Used in assistant chat turns so code blocks, headers, and lists render properly.
 */
export default function MarkdownRenderer({ content }: { content: string }) {
  return (
    <Markdown
      remarkPlugins={[remarkGfm]}
      components={{
        pre({ children }) {
          return (
            <pre className="overflow-x-auto rounded bg-zinc-100 p-2 text-[11.5px] leading-snug">
              {children}
            </pre>
          );
        },
        code({ className, children, ...props }) {
          const isBlock = className?.startsWith("language-");
          if (isBlock) {
            return (
              <code className={className} {...props}>
                {children}
              </code>
            );
          }
          return (
            <code
              className="rounded bg-zinc-100 px-1 py-0.5 text-[11.5px]"
              {...props}
            >
              {children}
            </code>
          );
        },
        table({ children }) {
          return (
            <div className="overflow-x-auto">
              <table className="border-collapse border border-zinc-300 text-[11.5px]">
                {children}
              </table>
            </div>
          );
        },
        th({ children }) {
          return (
            <th className="border border-zinc-300 bg-zinc-100 px-2 py-1 text-left font-semibold">
              {children}
            </th>
          );
        },
        td({ children }) {
          return (
            <td className="border border-zinc-300 px-2 py-1">{children}</td>
          );
        },
        ul({ children }) {
          return <ul className="list-disc pl-4">{children}</ul>;
        },
        ol({ children }) {
          return <ol className="list-decimal pl-4">{children}</ol>;
        },
        h1({ children }) {
          return <h1 className="mt-3 mb-1 text-lg font-bold">{children}</h1>;
        },
        h2({ children }) {
          return <h2 className="mt-2 mb-1 text-base font-bold">{children}</h2>;
        },
        h3({ children }) {
          return <h3 className="mt-2 mb-1 text-sm font-semibold">{children}</h3>;
        },
        blockquote({ children }) {
          return (
            <blockquote className="border-l-2 border-zinc-300 pl-2 text-zinc-500 italic">
              {children}
            </blockquote>
          );
        },
        a({ href, children }) {
          return (
            <a
              href={href}
              target="_blank"
              rel="noopener noreferrer"
              className="text-accent underline hover:text-accent/80"
            >
              {children}
            </a>
          );
        },
        hr() {
          return <hr className="my-2 border-zinc-200" />;
        },
      }}
    >
      {content}
    </Markdown>
  );
}
