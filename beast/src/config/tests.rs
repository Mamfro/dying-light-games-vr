use super::*;

#[test]
fn the_beast_offers_aer_and_afw_only() {
    assert_eq!(method(&Options::parse("stereo=afw")), StereoMethod::Afw);
    assert_eq!(method(&Options::parse("stereo=same")), StereoMethod::Aer, "not offered: AER");
    assert_eq!(method(&Options::parse("stereo=0")), StereoMethod::Aer, "stereo off is the probes-only switch");
    assert!(framegen_on(&Options::parse("framegen=fsr")) && !framegen_on(&Options::parse("framegen=1")));
}
