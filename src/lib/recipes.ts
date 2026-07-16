// Pure helpers for batch recipes (F25).

import type { Recipe, RecipeStep } from "../types";

export const RECIPE_VERSION = 1;

/** One-line description of a step for the builder list. */
export function describeRecipeStep(step: RecipeStep): string {
  switch (step.type) {
    case "reparse":
      return `Parse settings (${step.delimiter ?? "auto"} delimiter, ${step.encoding ?? "auto"} encoding)`;
    case "validateProfile":
      return `Validate against profile ${step.profileId}${step.failOnIssues ? " (fail on issues)" : ""}`;
    case "filter":
      return "Keep rows matching the captured filter";
    case "transform":
      return `Transform: ${step.spec.type}${
        step.columns && step.columns.length > 0
          ? ` on ${step.columns.join(", ")}`
          : " on all columns"
      }`;
    case "deduplicate":
      return `Deduplicate (keep ${step.keep})`;
    case "selectColumns":
      return `Keep columns: ${step.columns.join(", ")}`;
    case "sort":
      return `Sort by ${step.keys.map((k) => `${k.column}${k.descending ? " desc" : ""}`).join(", ")}`;
    case "export":
      return `Export (${step.options.delimiter === "," ? "CSV" : "delimited"}, ${step.options.encoding})`;
  }
}

/**
 * Parse a saved recipe file: JSON with the current version and a step list.
 * Unknown versions fail with a migration message instead of guessing.
 */
export function parseRecipeJson(text: string): Recipe {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    throw new Error("this is not a valid recipe file (bad JSON)");
  }
  const recipe = raw as Partial<Recipe>;
  if (typeof recipe.version !== "number" || !Array.isArray(recipe.steps)) {
    throw new Error("this is not a valid recipe file");
  }
  if (recipe.version !== RECIPE_VERSION) {
    throw new Error(
      `this recipe is version ${recipe.version}, but this CEESVEE understands version ${RECIPE_VERSION} — re-create it`,
    );
  }
  return {
    version: recipe.version,
    name: typeof recipe.name === "string" ? recipe.name : "recipe",
    steps: recipe.steps as RecipeStep[],
  };
}
