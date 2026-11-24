CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, workspaceId TEXT, createdAt INTEGER, lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER, recency INTEGER, checkpointAt INTEGER, value TEXT);
CREATE INDEX idx_composerHeaders_0 ON composerHeaders (workspaceId, isSubagent, isArchived, recency);
CREATE INDEX idx_composerHeaders_1 ON composerHeaders (recency, composerId);

INSERT INTO ItemTable VALUES ('cursor.startupMetrics.lastVersion', '3.31.15');
INSERT INTO composerHeaders (composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, isSubagent)
  VALUES ('6f1c2a9e-0b7d-4e5a-9c3f-2d8e1b4a7c60', '1795000000000', 1795000000000, 1795000300000, 0, 0);

INSERT INTO cursorDiskKV VALUES ('composerVirtualRowHeights:_recentIds', '{}');
INSERT INTO cursorDiskKV VALUES ('composerData:6f1c2a9e-0b7d-4e5a-9c3f-2d8e1b4a7c60', json_object(
  '_v', 17, 'composerId', '6f1c2a9e-0b7d-4e5a-9c3f-2d8e1b4a7c60', 'name', 'lorem ipsum dolor',
  'createdAt', 1795000000000, 'lastUpdatedAt', 1795000300000, 'status', 'completed',
  'generatingBubbleIds', json_array(), 'modelConfig', json_object('modelName', 'claude-4.5-sonnet', 'maxMode', json('false')),
  'fullConversationHeadersOnly', json_array(
    json_object('bubbleId', '0b1e6a2c-1f3d-4c5b-8a7e-9d0c1b2a3f40', 'type', 1),
    json_object('bubbleId', '1c2f7b3d-2e4a-4d6c-9b8f-0e1d2c3b4a51', 'type', 2),
    json_object('bubbleId', '2d3a8c4e-3f5b-4e7d-8c9a-1f2e3d4c5b62', 'type', 2),
    json_object('bubbleId', '3e4b9d5f-4a6c-4f8e-9dab-2a3f4e5d6c73', 'type', 2))));
INSERT INTO cursorDiskKV VALUES ('bubbleId:6f1c2a9e-0b7d-4e5a-9c3f-2d8e1b4a7c60:0b1e6a2c-1f3d-4c5b-8a7e-9d0c1b2a3f40', json_object(
  '_v', 3, 'type', 1, 'bubbleId', '0b1e6a2c-1f3d-4c5b-8a7e-9d0c1b2a3f40', 'text', 'sed do eiusmod tempor',
  'createdAt', '2026-11-18T20:26:40.000Z'));
INSERT INTO cursorDiskKV VALUES ('bubbleId:6f1c2a9e-0b7d-4e5a-9c3f-2d8e1b4a7c60:1c2f7b3d-2e4a-4d6c-9b8f-0e1d2c3b4a51', json_object(
  '_v', 3, 'type', 2, 'bubbleId', '1c2f7b3d-2e4a-4d6c-9b8f-0e1d2c3b4a51', 'text', '',
  'createdAt', '2026-11-18T20:26:52.000Z', 'thinking', json_object('text', 'ut labore et dolore'),
  'modelInfo', json_object('modelName', 'claude-4.5-sonnet-thinking'),
  'tokenCount', json_object('inputTokens', 18342, 'outputTokens', 412)));
INSERT INTO cursorDiskKV VALUES ('bubbleId:6f1c2a9e-0b7d-4e5a-9c3f-2d8e1b4a7c60:2d3a8c4e-3f5b-4e7d-8c9a-1f2e3d4c5b62', json_object(
  '_v', 3, 'type', 2, 'bubbleId', '2d3a8c4e-3f5b-4e7d-8c9a-1f2e3d4c5b62', 'text', '',
  'createdAt', '2026-11-18T20:27:03.000Z',
  'toolFormerData', json_object('tool', 15, 'name', 'read_file', 'status', 'completed', 'params', '{"targetFile":"src/lib.rs"}'),
  'tokenCount', json_object('inputTokens', 0, 'outputTokens', 0)));
INSERT INTO cursorDiskKV VALUES ('bubbleId:6f1c2a9e-0b7d-4e5a-9c3f-2d8e1b4a7c60:3e4b9d5f-4a6c-4f8e-9dab-2a3f4e5d6c73', json_object(
  '_v', 3, 'type', 2, 'bubbleId', '3e4b9d5f-4a6c-4f8e-9dab-2a3f4e5d6c73', 'text', 'magna aliqua ut enim ad minim',
  'createdAt', '2026-11-18T20:27:30.000Z',
  'modelInfo', json_object('modelName', 'claude-4.5-sonnet'),
  'tokenCount', json_object('inputTokens', 19107, 'outputTokens', 655)));

INSERT INTO cursorDiskKV VALUES ('composerData:9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d', json_object(
  '_v', 17, 'composerId', '9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d', 'name', 'quis nostrud',
  'createdAt', 1795100000000, 'lastUpdatedAt', 1795100060000, 'status', 'completed',
  'generatingBubbleIds', json_array(), 'modelConfig', json_object('modelName', 'gpt-5', 'maxMode', json('false')),
  'fullConversationHeadersOnly', json_array(
    json_object('bubbleId', '4f5cae60-5b7d-4a9f-8ebc-3b4a5f6e7d84', 'type', 1),
    json_object('bubbleId', '5a6dbf71-6c8e-4bae-9fcd-4c5b6a7f8e95', 'type', 2))));
INSERT INTO cursorDiskKV VALUES ('bubbleId:9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d:4f5cae60-5b7d-4a9f-8ebc-3b4a5f6e7d84', json_object(
  '_v', 3, 'type', 1, 'bubbleId', '4f5cae60-5b7d-4a9f-8ebc-3b4a5f6e7d84', 'text', 'exercitation ullamco',
  'createdAt', '2026-11-20T00:13:20.000Z'));
INSERT INTO cursorDiskKV VALUES ('bubbleId:9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d:5a6dbf71-6c8e-4bae-9fcd-4c5b6a7f8e95', json_object(
  '_v', 3, 'type', 2, 'bubbleId', '5a6dbf71-6c8e-4bae-9fcd-4c5b6a7f8e95', 'text', 'laboris nisi ut aliquip',
  'createdAt', '2026-11-20T00:13:41.000Z',
  'tokenCount', json_object('inputTokens', 5230, 'outputTokens', 1180)));
