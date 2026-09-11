-- 查询权限独立于模型权限；旧密钥默认无知识库权限。
CREATE TABLE api_key_knowledge_access (
    api_key_id TEXT NOT NULL REFERENCES api_keys(id) ON DELETE CASCADE,
    kb_id TEXT NOT NULL REFERENCES kb_knowledge_bases(id) ON DELETE CASCADE,
    PRIMARY KEY (api_key_id, kb_id)
);
