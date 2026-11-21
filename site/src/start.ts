import type { Painter } from "./heat/field";
import { must, type Scene } from "./scene";

/** How long a button says what happened before it reads "Copy" again. */
const SAID_FOR = 2000;

/**
 * The install steps: each command block gets a working copy button, and a
 * copy leaves a touch of warmth under it. Where the browser gives no
 * clipboard the buttons stay hidden; the commands are plain text either way.
 * A block holds one command, so a paste never types into the prompt of the
 * command before it.
 */
export function createStart(root: HTMLElement): Scene {
  const status = must<HTMLElement>(root, "[data-copy-status]");
  /** Buttons whose copy the field has not absorbed yet. */
  const warm: HTMLElement[] = [];

  const blocks = navigator.clipboard ? Array.from(root.querySelectorAll<HTMLElement>("[data-copy]")) : [];
  for (const block of blocks) {
    const button = must<HTMLButtonElement>(block, "button");
    const what = (button.getAttribute("aria-label") ?? "Copy the command").replace(/^Copy /, "");
    const command = must<HTMLElement>(block, ".cmd code");
    let reset = 0;

    const say = (label: string, announcement: string, done: boolean): void => {
      button.textContent = label;
      button.classList.toggle("is-done", done);
      status.textContent = announcement;
      window.clearTimeout(reset);
      reset = window.setTimeout(() => {
        button.textContent = "Copy";
        button.classList.remove("is-done");
      }, SAID_FOR);
    };

    button.hidden = false;
    button.addEventListener("click", () => {
      const text = (command.textContent ?? "").replace(/\s+/g, " ").trim();
      navigator.clipboard.writeText(text).then(
        () => {
          warm.push(button);
          say("Copied", `Copied ${what}.`, true);
        },
        () => {
          // The browser refused: select the text, so one more key press copies it.
          window.getSelection()?.selectAllChildren(command);
          say("Selected", `Could not copy. Selected ${what} instead.`, false);
        },
      );
    });
  }

  return {
    el: root,
    get animating(): boolean {
      return warm.length > 0;
    },
    paint(painter: Painter): void {
      for (const button of warm) {
        const r = button.getBoundingClientRect();
        painter.pulse(r.left + r.width / 2, r.top + r.height / 2, 64, 0.5);
      }
      warm.length = 0;
    },
    update(): void {},
  };
}
