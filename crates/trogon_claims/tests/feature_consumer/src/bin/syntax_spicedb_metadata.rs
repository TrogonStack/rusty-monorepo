use trogon_claims_syntax::SyntaxRule::IdentifierAlphabet;
use trogon_claims_syntax::SyntaxRule::IdentifierStart;
use trogon_claims_syntax::SyntaxRule::MaxBytes;
use trogon_claims_syntax::SyntaxRule::NotWildcard;
use trogon_claims_syntax::SyntaxRule::ObjectIdAlphabet;
use trogon_claims_syntax::SyntaxRule::SegmentAlphabet;
use trogon_claims_syntax::SyntaxRule::SegmentEnd;
use trogon_claims_syntax::SyntaxRule::SegmentLength;
use trogon_claims_syntax::SyntaxRule::SegmentStart;
use trogon_claims_syntax::OBJECT_ID_ALPHABET;
use trogon_claims_syntax::OBJECT_ID_MAX_BYTES;
use trogon_claims_syntax::OBJECT_TYPE_MAX_BYTES;
use trogon_claims_syntax::PARAM_NAME_MAX_BYTES;
use trogon_claims_syntax::RELATION_NAME_MAX_BYTES;
use trogon_claims_syntax::SEGMENT_MIN_BYTES;
use trogon_claims_syntax::TYPE_FINAL_SEGMENT_MAX_BYTES;
use trogon_claims_syntax::TYPE_INNER_SEGMENT_MAX_BYTES;
use trogon_claims_syntax::WILDCARD_BYTE;

fn main() {
    assert_eq!(OBJECT_ID_ALPHABET, "[a-zA-Z0-9/_|\\-=+]");
    assert_eq!(OBJECT_ID_MAX_BYTES, 1024);
    assert_eq!(OBJECT_TYPE_MAX_BYTES, 128);
    assert_eq!(PARAM_NAME_MAX_BYTES, 64);
    assert_eq!(RELATION_NAME_MAX_BYTES, 64);
    assert_eq!(SEGMENT_MIN_BYTES, 3);
    assert_eq!(TYPE_FINAL_SEGMENT_MAX_BYTES, 64);
    assert_eq!(TYPE_INNER_SEGMENT_MAX_BYTES, 63);
    assert_eq!(WILDCARD_BYTE, b'*');
    let rules = [
        MaxBytes(1024),
        ObjectIdAlphabet,
        NotWildcard,
        SegmentAlphabet,
        SegmentStart,
        SegmentEnd,
        SegmentLength { min: 3, max: 64 },
        IdentifierStart,
        IdentifierAlphabet,
    ];
    assert_eq!(rules.len(), 9);
}
