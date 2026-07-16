// The central typed command registry (F11).
//
// Every user-invokable action is described once — id, title, keywords,
// category, default shortcut, availability, and execution — and consumed by
// the command palette, the global shortcut dispatcher, and the shortcut
// editor. Commands must NOT be defined ad hoc in components.

import { fuzzyScore } from "./fuzzy";

export type CommandCategory =
  | "File"
  | "Edit"
  | "View"
  | "Data"
  | "Export"
  | "Tabs"
  | "Navigate"
  | "Help";

export interface AppCommand {
  /** Stable id (never rename: shortcut overrides are keyed by it). */
  id: string;
  title: string;
  /** Extra search terms beyond the title. */
  keywords?: string[];
  category: CommandCategory;
  /** Normalized default binding ("mod+shift+s"), if any. */
  defaultShortcut?: string;
  /**
   * Why the command is currently unavailable, or null when it can run.
   * Unavailable commands stay visible in the palette with this explanation.
   */
  unavailableReason?: () => string | null;
  run: () => void;
  /**
   * When set, the palette collects a free-text argument (showing this
   * placeholder) and calls `runWith` instead of `run`. `runWith` returns an
   * error message to display, or null on success.
   */
  argPlaceholder?: string;
  runWith?: (arg: string) => string | null;
  /** Allow the shortcut to fire while focus is in an input/textarea. */
  allowInEditable?: boolean;
}

/** A palette entry scored against the current query. */
export interface RankedCommand {
  command: AppCommand;
  score: number;
  unavailable: string | null;
}

/**
 * Registry of static commands plus dynamic providers (recent files, open
 * tabs) whose entries are regenerated each time the palette opens.
 */
export class CommandRegistry {
  private commands = new Map<string, AppCommand>();
  private providers: Array<() => AppCommand[]> = [];

  register(commands: AppCommand[]): void {
    for (const command of commands) {
      if (this.commands.has(command.id)) {
        throw new Error(`duplicate command id: ${command.id}`);
      }
      this.commands.set(command.id, command);
    }
  }

  /** Register a generator of transient commands (tabs, recents). */
  addProvider(provider: () => AppCommand[]): void {
    this.providers.push(provider);
  }

  byId(id: string): AppCommand | undefined {
    return this.commands.get(id);
  }

  /** Static commands only — the set shortcuts can bind to. */
  staticCommands(): AppCommand[] {
    return [...this.commands.values()];
  }

  /** Static + dynamic commands, for the palette. */
  allCommands(): AppCommand[] {
    const dynamic = this.providers.flatMap((provider) => provider());
    return [...this.commands.values(), ...dynamic];
  }

  /** Default bindings keyed by command id. */
  defaultBindings(): Map<string, string> {
    const out = new Map<string, string>();
    for (const command of this.commands.values()) {
      if (command.defaultShortcut) out.set(command.id, command.defaultShortcut);
    }
    return out;
  }

  /**
   * Rank commands for a palette query. Empty query lists everything in
   * registration order; otherwise fuzzy-scored best-first. Unavailable
   * commands are included (annotated) so users learn why, but rank below
   * available ones at equal score.
   */
  search(query: string): RankedCommand[] {
    const ranked: RankedCommand[] = [];
    for (const command of this.allCommands()) {
      const score = query === "" ? 0 : fuzzyScore(query, command.title, command.keywords);
      if (score === null) continue;
      ranked.push({
        command,
        score,
        unavailable: command.unavailableReason?.() ?? null,
      });
    }
    if (query !== "") {
      ranked.sort((a, b) => {
        if (a.score !== b.score) return b.score - a.score;
        if (!a.unavailable !== !b.unavailable) return a.unavailable ? 1 : -1;
        return a.command.title.localeCompare(b.command.title);
      });
    }
    return ranked;
  }
}

/** The app-wide registry instance (populated in commandDefs.ts). */
export const registry = new CommandRegistry();
