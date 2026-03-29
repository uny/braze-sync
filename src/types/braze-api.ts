// Catalogs API

export interface BrazeCatalogField {
  name: string;
  type: "string" | "number" | "boolean" | "time";
}

export interface BrazeCatalog {
  name: string;
  description: string;
  fields: BrazeCatalogField[];
  num_items: number;
  created_at: string;
  updated_at: string;
}

export interface BrazeCatalogListResponse {
  catalogs: BrazeCatalog[];
  message: string;
}

export interface BrazeCatalogCreateRequest {
  catalogs: Array<{
    name: string;
    description: string;
    fields: BrazeCatalogField[];
  }>;
}

export interface BrazeCatalogCreateFieldsRequest {
  fields: BrazeCatalogField[];
}

// Content Blocks API

export interface BrazeContentBlockListItem {
  content_block_id: string;
  name: string;
  created_at: string;
  last_edited: string;
}

export interface BrazeContentBlockListResponse {
  count: number;
  content_blocks: BrazeContentBlockListItem[];
  message: string;
}

export interface BrazeContentBlockInfo {
  content_block_id: string;
  name: string;
  content: string;
  description: string;
  state: "active" | "draft";
  tags: string[];
  created_at: string;
  last_edited: string;
  message: string;
}

export interface BrazeContentBlockCreateRequest {
  name: string;
  content: string;
  description?: string;
  state?: "active" | "draft";
  tags?: string[];
}

export interface BrazeContentBlockUpdateRequest {
  content_block_id: string;
  name: string;
  content: string;
  description?: string;
  state?: "active" | "draft";
  tags?: string[];
}

// Generic API response
export interface BrazeApiResponse {
  message: string;
  errors?: string[];
}
