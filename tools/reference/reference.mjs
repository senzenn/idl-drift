// Reference decode with @coral-xyz/anchor's BorshCoder, for `idl-verify --reference`.
// Adapted from the Shipstern parity harness (#338).
//
//   npm install
//   node reference.mjs <idl.json> <samples.json> > reference.json
//
// Accounts are classified by discriminator, not by the samples' `type` label.
// Anchor's coder ignores `serialization` and `repr`: it decodes zero-copy types as Borsh,
// so its values are wrong after any repr(C) padding. idl-verify skips those types.
import fs from 'node:fs';
import anchor from '@coral-xyz/anchor';

const { BorshCoder, BN, web3 } = anchor;
const [idlPath, samplesPath] = process.argv.slice(2);
if (!idlPath || !samplesPath) {
    process.stderr.write('usage: node reference.mjs <idl.json> <samples.json>\n');
    process.exit(2);
}

// BN, PublicKey and Buffer have no stable JSON form; flatten them.
const plain = v => {
    if (v === null || v === undefined) return null;
    if (BN.isBN(v)) return { bn: v.toString(10) };
    if (v instanceof web3.PublicKey) return v.toBase58();
    if (Buffer.isBuffer(v) || v instanceof Uint8Array) return [...v];
    if (typeof v === 'number') return Number.isNaN(v) ? { nan: true } : v;
    if (Array.isArray(v)) return v.map(plain);
    if (typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, plain(x)]));
    return v;
};

const idl = JSON.parse(fs.readFileSync(idlPath, 'utf8'));
const sample = JSON.parse(fs.readFileSync(samplesPath, 'utf8'));
const coder = new BorshCoder(idl);

const attempt = fn => {
    try { return { status: 'ok', value: plain(fn()) }; }
    catch (e) { return { status: 'error', error: String(e.message ?? e).slice(0, 200) }; }
};

// Longest discriminator first, so a short one never shadows a longer match.
const accountTypes = (idl.accounts ?? [])
    .filter(a => a.discriminator?.length)
    .sort((a, b) => b.discriminator.length - a.discriminator.length);
const classify = data => accountTypes.find(a => a.discriminator.every((byte, i) => data[i] === byte))?.name ?? null;

const accounts = (sample.accounts ?? []).map(a => {
    const data = Buffer.from(a.data, 'base64');
    const type = classify(data);
    return {
        address: a.address,
        type,
        reference: attempt(() => {
            if (!type) throw new Error('no account discriminator matched');
            return coder.accounts.decode(type, data);
        }),
    };
});

const instructions = (sample.instructions ?? []).map(ix => ({
    signature: ix.signature,
    reference: attempt(() => {
        const decoded = coder.instruction.decode(Buffer.from(ix.data, 'base64'));
        if (!decoded) throw new Error('no instruction matched');
        return decoded;
    }),
}));

process.stdout.write(JSON.stringify({ programId: sample.programId, accounts, instructions }, null, 1) + '\n');
