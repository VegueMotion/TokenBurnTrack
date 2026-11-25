CREATE TABLE gen_metadata (idx integer, data blob, size integer);
CREATE TABLE trajectory_metadata_blob (id text, data blob);
CREATE TABLE steps (idx integer, step_type integer, metadata blob);

INSERT INTO trajectory_metadata_blob VALUES ('8e2d4c6b-1a3f-4b5e-9c7d-0e1f2a3b4c5d', X'0a1f0a1d66696c653a2f2f2f752f757365722f6b65656e2d6c61676f6f6e2d3435120608a0d785d806');
-- no label, only the response model; timestamp from the step with generation index 0
INSERT INTO gen_metadata VALUES (0, X'0a3b9a011667656d696e692d332e312d70726f2d70726576696577222008870810a01f18e80728c41348bc0550ac025a0c726573705f394b7034794434', 61);
-- no label and no step: falls back to the conversation's created-at
INSERT INTO gen_metadata VALUES (1, X'0a389a011667656d696e692d332e312d70726f2d70726576696577221d08870810fa01185028a01f480050505a0c726573705f394b7034794535', 58);
INSERT INTO steps VALUES (0, 15, X'0a0608b4d785d806a201021800');
