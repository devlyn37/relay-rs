CREATE INDEX idx_requests_id ON requests (id);
CREATE INDEX idx_requests_chain_mined ON requests (chain, mined);