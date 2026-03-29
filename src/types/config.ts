export interface Environment {
	api_url: string;
	api_key_env: string;
}

export interface ResourcePaths {
	catalogs?: string;
	content_blocks?: string;
	custom_attributes?: string;
	email_templates?: string;
}

export interface Config {
	version: number;
	environments: Record<string, Environment>;
	resources: ResourcePaths;
}
