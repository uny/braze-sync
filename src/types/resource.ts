export interface CatalogFieldDefinition {
  name: string;
  type: "string" | "number" | "boolean" | "time";
}

export interface CatalogDefinition {
  name: string;
  description: string;
  fields: CatalogFieldDefinition[];
}

export interface ContentBlockDefinition {
  name: string;
  content: string;
  description?: string;
  state?: "active" | "draft";
  tags?: string[];
}

export interface LocalFileOutput {
  path: string;
  content: string;
}
