// Copyright 2017 pdb Developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use std::fmt;

use scroll::{ctx::TryFromCtx, Endian, Pread, LE};

use crate::common::*;
use crate::msf::*;
use crate::FallibleIterator;
use crate::SectionCharacteristics;

mod annotations;
mod constants;

use self::constants::*;
pub use self::constants::{CPUType, SourceLanguage};

pub use self::annotations::*;

/// The raw type discriminator for `Symbols`.
pub type SymbolKind = u16;

/// Represents a symbol from the symbol table.
///
/// A `Symbol` is represented internally as a `&[u8]`, and in general the bytes inside are not
/// inspected in any way before calling any of the accessor methods.
///
/// To avoid copying, `Symbol`s exist as references to data owned by the parent `SymbolTable`.
/// Therefore, a `Symbol` may not outlive its parent `SymbolTable`.
#[derive(Copy, Clone, PartialEq)]
pub struct Symbol<'t> {
    index: SymbolIndex,
    data: &'t [u8],
}

impl<'t> Symbol<'t> {
    /// The index of this symbol in the containing symbol stream.
    #[inline]
    #[must_use]
    pub fn index(&self) -> SymbolIndex {
        self.index
    }

    /// Returns the kind of symbol identified by this Symbol.
    #[inline]
    #[must_use]
    pub fn raw_kind(&self) -> SymbolKind {
        debug_assert!(self.data.len() >= 2);
        self.data.pread_with(0, LE).unwrap_or_default()
    }

    /// Returns the raw bytes of this symbol record, including the symbol type and extra data, but
    /// not including the preceding symbol length indicator.
    #[inline]
    #[must_use]
    pub fn raw_bytes(&self) -> &'t [u8] {
        self.data
    }

    /// Parse the symbol into the `SymbolData` it contains.
    #[inline]
    pub fn parse(&self) -> Result<SymbolData> {
        self.raw_bytes().pread_with(0, ())
    }

    /// Returns whether this symbol starts a scope.
    ///
    /// If `true`, this symbol has a `parent` and an `end` field, which contains the offset of the
    /// corrsponding end symbol.
    #[must_use]
    pub fn starts_scope(&self) -> bool {
        matches!(
            self.raw_kind(),
            S_GPROC16
                | S_GPROC32
                | S_GPROC32_ST
                | S_GPROCMIPS
                | S_GPROCMIPS_ST
                | S_GPROCIA64
                | S_GPROCIA64_ST
                | S_LPROC16
                | S_LPROC32
                | S_LPROC32_ST
                | S_LPROC32_DPC
                | S_LPROCMIPS
                | S_LPROCMIPS_ST
                | S_LPROCIA64
                | S_LPROCIA64_ST
                | S_LPROC32_DPC_ID
                | S_GPROC32_ID
                | S_GPROCMIPS_ID
                | S_GPROCIA64_ID
                | S_BLOCK16
                | S_BLOCK32
                | S_BLOCK32_ST
                | S_WITH16
                | S_WITH32
                | S_WITH32_ST
                | S_THUNK16
                | S_THUNK32
                | S_THUNK32_ST
                | S_SEPCODE
                | S_GMANPROC
                | S_GMANPROC_ST
                | S_LMANPROC
                | S_LMANPROC_ST
                | S_INLINESITE
                | S_INLINESITE2
        )
    }

    /// Returns whether this symbol declares the end of a scope.
    #[must_use]
    pub fn ends_scope(&self) -> bool {
        matches!(self.raw_kind(), S_END | S_PROC_ID_END | S_INLINESITE_END)
    }
}

impl fmt::Debug for Symbol<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Symbol{{ kind: 0x{:x} [{} bytes] }}",
            self.raw_kind(),
            self.data.len()
        )
    }
}

fn parse_symbol_name<'t>(buf: &mut ParseBuffer<'t>, kind: SymbolKind) -> Result<RawString<'t>> {
    if kind < S_ST_MAX {
        // Pascal-style name
        buf.parse_u8_pascal_string()
    } else {
        // NUL-terminated name
        buf.parse_cstring()
    }
}

fn parse_optional_name<'t>(
    buf: &mut ParseBuffer<'t>,
    kind: SymbolKind,
) -> Result<Option<RawString<'t>>> {
    if kind < S_ST_MAX {
        // ST variants do not specify a name
        Ok(None)
    } else {
        // NUL-terminated name
        buf.parse_cstring().map(Some)
    }
}

fn parse_optional_index(buf: &mut ParseBuffer<'_>) -> Result<Option<SymbolIndex>> {
    Ok(match buf.parse()? {
        SymbolIndex(0) => None,
        index => Some(index),
    })
}

// data types are defined at:
//   https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L3038
// constants defined at:
//   https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L2735
// decoding reference:
//   https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/cvdump/dumpsym7.cpp#L264

/// Information parsed from a [`Symbol`] record.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SymbolData {
    /// End of a scope, such as a procedure.
    ScopeEnd,
    /// Name of the object file of this module.
    ObjName(ObjNameSymbol),
    /// A Register variable.
    RegisterVariable(RegisterVariableSymbol),
    /// A constant value.
    Constant(ConstantSymbol),
    /// A user defined type.
    UserDefinedType(UserDefinedTypeSymbol),
    /// A Register variable spanning multiple registers.
    MultiRegisterVariable(MultiRegisterVariableSymbol),
    /// Static data, such as a global variable.
    Data(DataSymbol),
    /// A public symbol with a mangled name.
    Public(PublicSymbol),
    /// A procedure, such as a function or method.
    Procedure(ProcedureSymbol),
    /// A managed procedure, such as a function or method.
    ManagedProcedure(ManagedProcedureSymbol),
    /// A thread local variable.
    ThreadStorage(ThreadStorageSymbol),
    /// Flags used to compile a module.
    CompileFlags(CompileFlagsSymbol),
    /// A using namespace directive.
    UsingNamespace(UsingNamespaceSymbol),
    /// Reference to a [`ProcedureSymbol`].
    ProcedureReference(ProcedureReferenceSymbol),
    /// Reference to an imported variable.
    DataReference(DataReferenceSymbol),
    /// Reference to an annotation.
    AnnotationReference(AnnotationReferenceSymbol),
    /// Reference to a managed procedure.
    TokenReference(TokenReferenceSymbol),
    /// Trampoline thunk.
    Trampoline(TrampolineSymbol),
    /// An exported symbol.
    Export(ExportSymbol),
    /// A local symbol in optimized code.
    Local(LocalSymbol),
    /// A managed local variable slot.
    ManagedSlot(ManagedSlotSymbol),
    /// Reference to build information.
    BuildInfo(BuildInfoSymbol),
    /// The callsite of an inlined function.
    InlineSite(InlineSiteSymbol),
    /// End of an inline callsite.
    InlineSiteEnd,
    /// End of a procedure.
    ProcedureEnd,
    /// A label.
    Label(LabelSymbol),
    /// A block.
    Block(BlockSymbol),
    /// Data allocated relative to a register.
    RegisterRelative(RegisterRelativeSymbol),
    /// A thunk.
    Thunk(ThunkSymbol),
    /// A block of separated code.
    SeparatedCode(SeparatedCodeSymbol),
    /// OEM information.
    OEM(OemSymbol),
    /// Environment block split off from `S_COMPILE2`.
    EnvBlock(EnvBlockSymbol),
    /// A COFF section in a PE executable.
    Section(SectionSymbol),
    /// A COFF group.
    CoffGroup(CoffGroupSymbol),
    /// A live range of a variable.
    DefRange(DefRangeSymbol),
    /// A live range of a sub field of a variable.
    DefRangeSubField(DefRangeSubFieldSymbol),
    /// A live range of a register variable.
    DefRangeRegister(DefRangeRegisterSymbol),
    /// A live range of a frame pointer-relative variable.
    DefRangeFramePointerRelative(DefRangeFramePointerRelativeSymbol),
    /// A frame-pointer variable which is valid in the full scope of the function.
    DefRangeFramePointerRelativeFullScope(DefRangeFramePointerRelativeFullScopeSymbol),
    /// A live range of a sub field of a register variable.
    DefRangeSubFieldRegister(DefRangeSubFieldRegisterSymbol),
    /// A live range of a variable related to a register.
    DefRangeRegisterRelative(DefRangeRegisterRelativeSymbol),
    /// A base pointer-relative variable.
    BasePointerRelative(BasePointerRelativeSymbol),
    /// Extra frame and proc information.
    FrameProcedure(FrameProcedureSymbol),
    /// Indirect call site information.
    CallSiteInfo(CallSiteInfoSymbol),
    /// Callers of a function.
    Callers(FunctionListSymbol),
    /// Callees of a function.
    Callees(FunctionListSymbol),
    /// Inlinees of a function.
    Inlinees(InlineesSymbol),
    /// Describes the layout of a jump table
    ArmSwitchTable(ArmSwitchTableSymbol),
    /// Heap allocation site
    HeapAllocationSite(HeapAllocationSiteSymbol),
    /// A security cookie on a stack frame
    FrameCookie(FrameCookieSymbol),
}

impl SymbolData {
    /// Returns the name of this symbol if it has one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::ObjName(data) => Some(&data.name),
            Self::Constant(data) => Some(&data.name),
            Self::UserDefinedType(data) => Some(&data.name),
            Self::Data(data) => Some(&data.name),
            Self::Public(data) => Some(&data.name),
            Self::Procedure(data) => Some(&data.name),
            Self::ManagedProcedure(data) => data.name.as_deref(),
            Self::ThreadStorage(data) => Some(&data.name),
            Self::UsingNamespace(data) => Some(&data.name),
            Self::ProcedureReference(data) => data.name.as_deref(),
            Self::DataReference(data) => data.name.as_deref(),
            Self::AnnotationReference(data) => Some(&data.name),
            Self::TokenReference(data) => Some(&data.name),
            Self::Export(data) => Some(&data.name),
            Self::Local(data) => Some(&data.name),
            Self::ManagedSlot(data) => Some(&data.name),
            Self::Label(data) => Some(&data.name),
            Self::Block(data) => Some(&data.name),
            Self::RegisterRelative(data) => Some(&data.name),
            Self::Thunk(data) => Some(&data.name),
            Self::Section(data) => Some(&data.name),
            Self::CoffGroup(data) => Some(&data.name),
            Self::BasePointerRelative(data) => Some(&data.name),
            Self::ScopeEnd
            | Self::RegisterVariable(_)
            | Self::MultiRegisterVariable(_)
            | Self::CompileFlags(_)
            | Self::Trampoline(_)
            | Self::InlineSite(_)
            | Self::BuildInfo(_)
            | Self::InlineSiteEnd
            | Self::ProcedureEnd
            | Self::SeparatedCode(_)
            | Self::OEM(_)
            | Self::EnvBlock(_)
            | Self::DefRange(_)
            | Self::DefRangeSubField(_)
            | Self::DefRangeRegister(_)
            | Self::DefRangeFramePointerRelative(_)
            | Self::DefRangeFramePointerRelativeFullScope(_)
            | Self::DefRangeSubFieldRegister(_)
            | Self::DefRangeRegisterRelative(_)
            | Self::FrameProcedure(_)
            | Self::CallSiteInfo(_)
            | Self::Callers(_)
            | Self::Callees(_)
            | Self::Inlinees(_)
            | Self::ArmSwitchTable(_)
            | Self::HeapAllocationSite(_)
            | Self::FrameCookie(_) => None,
        }
    }
}

impl<'t> TryFromCtx<'t> for SymbolData {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], _ctx: ()) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);
        let kind = buf.parse()?;

        let symbol = match kind {
            S_END => SymbolData::ScopeEnd,
            S_OBJNAME | S_OBJNAME_ST => SymbolData::ObjName(buf.parse_with(kind)?),
            S_REGISTER | S_REGISTER_ST => SymbolData::RegisterVariable(buf.parse_with(kind)?),
            S_CONSTANT | S_CONSTANT_ST | S_MANCONSTANT => {
                SymbolData::Constant(buf.parse_with(kind)?)
            }
            S_UDT | S_UDT_ST | S_COBOLUDT | S_COBOLUDT_ST => {
                SymbolData::UserDefinedType(buf.parse_with(kind)?)
            }
            S_MANYREG | S_MANYREG_ST | S_MANYREG2 | S_MANYREG2_ST => {
                SymbolData::MultiRegisterVariable(buf.parse_with(kind)?)
            }
            S_LDATA32 | S_LDATA32_ST | S_GDATA32 | S_GDATA32_ST | S_LMANDATA | S_LMANDATA_ST
            | S_GMANDATA | S_GMANDATA_ST => SymbolData::Data(buf.parse_with(kind)?),
            S_PUB32 | S_PUB32_ST => SymbolData::Public(buf.parse_with(kind)?),
            S_LPROC32 | S_LPROC32_ST | S_GPROC32 | S_GPROC32_ST | S_LPROC32_ID | S_GPROC32_ID
            | S_LPROC32_DPC | S_LPROC32_DPC_ID => SymbolData::Procedure(buf.parse_with(kind)?),
            S_LMANPROC | S_GMANPROC => SymbolData::ManagedProcedure(buf.parse_with(kind)?),
            S_LTHREAD32 | S_LTHREAD32_ST | S_GTHREAD32 | S_GTHREAD32_ST => {
                SymbolData::ThreadStorage(buf.parse_with(kind)?)
            }
            S_COMPILE2 | S_COMPILE2_ST | S_COMPILE3 => {
                SymbolData::CompileFlags(buf.parse_with(kind)?)
            }
            S_UNAMESPACE | S_UNAMESPACE_ST => SymbolData::UsingNamespace(buf.parse_with(kind)?),
            S_PROCREF | S_PROCREF_ST | S_LPROCREF | S_LPROCREF_ST => {
                SymbolData::ProcedureReference(buf.parse_with(kind)?)
            }
            S_TRAMPOLINE => Self::Trampoline(buf.parse_with(kind)?),
            S_DATAREF | S_DATAREF_ST => SymbolData::DataReference(buf.parse_with(kind)?),
            S_ANNOTATIONREF => SymbolData::AnnotationReference(buf.parse_with(kind)?),
            S_TOKENREF => SymbolData::TokenReference(buf.parse_with(kind)?),
            S_EXPORT => SymbolData::Export(buf.parse_with(kind)?),
            S_LOCAL => SymbolData::Local(buf.parse_with(kind)?),
            S_MANSLOT | S_MANSLOT_ST => SymbolData::ManagedSlot(buf.parse_with(kind)?),
            S_BUILDINFO => SymbolData::BuildInfo(buf.parse_with(kind)?),
            S_INLINESITE | S_INLINESITE2 => SymbolData::InlineSite(buf.parse_with(kind)?),
            S_INLINESITE_END => SymbolData::InlineSiteEnd,
            S_PROC_ID_END => SymbolData::ProcedureEnd,
            S_LABEL32 | S_LABEL32_ST => SymbolData::Label(buf.parse_with(kind)?),
            S_BLOCK32 | S_BLOCK32_ST => SymbolData::Block(buf.parse_with(kind)?),
            S_REGREL32 => SymbolData::RegisterRelative(buf.parse_with(kind)?),
            S_THUNK32 | S_THUNK32_ST => SymbolData::Thunk(buf.parse_with(kind)?),
            S_SEPCODE => SymbolData::SeparatedCode(buf.parse_with(kind)?),
            S_OEM => SymbolData::OEM(buf.parse_with(kind)?),
            S_ENVBLOCK => SymbolData::EnvBlock(buf.parse_with(kind)?),
            S_SECTION => SymbolData::Section(buf.parse_with(kind)?),
            S_COFFGROUP => SymbolData::CoffGroup(buf.parse_with(kind)?),
            S_DEFRANGE => SymbolData::DefRange(buf.parse_with(kind)?),
            S_DEFRANGE_SUBFIELD => SymbolData::DefRangeSubField(buf.parse_with(kind)?),
            S_DEFRANGE_REGISTER => SymbolData::DefRangeRegister(buf.parse_with(kind)?),
            S_DEFRANGE_FRAMEPOINTER_REL => {
                SymbolData::DefRangeFramePointerRelative(buf.parse_with(kind)?)
            }
            S_DEFRANGE_FRAMEPOINTER_REL_FULL_SCOPE => {
                SymbolData::DefRangeFramePointerRelativeFullScope(buf.parse_with(kind)?)
            }
            S_DEFRANGE_SUBFIELD_REGISTER => {
                SymbolData::DefRangeSubFieldRegister(buf.parse_with(kind)?)
            }
            S_DEFRANGE_REGISTER_REL => SymbolData::DefRangeRegisterRelative(buf.parse_with(kind)?),
            S_BPREL32 | S_BPREL32_ST | S_BPREL32_16T => {
                SymbolData::BasePointerRelative(buf.parse_with(kind)?)
            }
            S_FRAMEPROC => SymbolData::FrameProcedure(buf.parse_with(kind)?),
            S_CALLSITEINFO => SymbolData::CallSiteInfo(buf.parse_with(kind)?),
            S_CALLERS => SymbolData::Callers(buf.parse_with(kind)?),
            S_CALLEES => SymbolData::Callees(buf.parse_with(kind)?),
            S_INLINEES => SymbolData::Inlinees(buf.parse_with(kind)?),
            S_ARMSWITCHTABLE => SymbolData::ArmSwitchTable(buf.parse_with(kind)?),
            S_HEAPALLOCSITE => SymbolData::HeapAllocationSite(buf.parse_with(kind)?),
            S_FRAMECOOKIE => SymbolData::FrameCookie(buf.parse_with(kind)?),
            other => return Err(Error::UnimplementedSymbolKind(other)),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A Register variable.
///
/// Symbol kind `S_REGISTER`, or `S_REGISTER_ST`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterVariableSymbol {
    /// Identifier of the variable type.
    pub type_index: TypeIndex,
    /// The register this variable is stored in.
    pub register: Register,
    /// Name of the variable.
    pub name: String,
    /// Parameter slot
    pub slot: Option<i32>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for RegisterVariableSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let type_index: TypeIndex = buf.parse()?;
        let register: Register = buf.parse()?;
        let name: RawString<'t> = parse_symbol_name(&mut buf, kind)?;

        let slot: Option<i32> = if (this.len() as i64 - name.len() as i64 - 8i64) >= 6 {
            if this[name.len() + 0xb] == 0x24 {
                Some(ParseBuffer::from(&this[(name.len() + 0xc)..]).parse()?)
            } else {
                None
            }
        } else {
            None
        };

        Ok((
            Self {
                type_index,
                register,
                name: name.to_string().to_string(),
                slot,
            },
            buf.pos(),
        ))
    }
}

/// A Register variable spanning multiple registers.
///
/// Symbol kind `S_MANYREG`, `S_MANYREG_ST`, `S_MANYREG2`, or `S_MANYREG2_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiRegisterVariableSymbol {
    /// Identifier of the variable type.
    pub type_index: TypeIndex,
    /// Most significant register first.
    pub registers: Vec<(Register, String)>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for MultiRegisterVariableSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let type_index = buf.parse()?;
        let count = match kind {
            S_MANYREG2 | S_MANYREG2_ST => buf.parse::<u16>()?,
            _ => u16::from(buf.parse::<u8>()?),
        };

        let mut registers = Vec::with_capacity(count as usize);
        for _ in 0..count {
            registers.push((
                buf.parse()?,
                parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
            ));
        }

        let symbol = MultiRegisterVariableSymbol {
            type_index,
            registers,
        };

        Ok((symbol, buf.pos()))
    }
}

// CV_PUBSYMFLAGS_e
const CVPSF_CODE: u32 = 0x1;
const CVPSF_FUNCTION: u32 = 0x2;
const CVPSF_MANAGED: u32 = 0x4;
const CVPSF_MSIL: u32 = 0x8;

/// A public symbol with a mangled name.
///
/// Symbol kind `S_PUB32`, or `S_PUB32_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicSymbol {
    /// The public symbol refers to executable code.
    pub code: bool,
    /// The public symbol is a function.
    pub function: bool,
    /// The symbol is in managed code (native or IL).
    pub managed: bool,
    /// The symbol is managed IL code.
    pub msil: bool,
    /// Start offset of the symbol.
    pub offset: PdbInternalSectionOffset,
    /// Mangled name of the symbol.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for PublicSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let flags = buf.parse::<u32>()?;
        let symbol = PublicSymbol {
            code: flags & CVPSF_CODE != 0,
            function: flags & CVPSF_FUNCTION != 0,
            managed: flags & CVPSF_MANAGED != 0,
            msil: flags & CVPSF_MSIL != 0,
            offset: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// Static data, such as a global variable.
///
/// Symbol kinds:
///  - `S_LDATA32` and `S_LDATA32_ST` for local unmanaged data
///  - `S_GDATA32` and `S_GDATA32_ST` for global unmanaged data
///  - `S_LMANDATA32` and `S_LMANDATA32_ST` for local managed data
///  - `S_GMANDATA32` and `S_GMANDATA32_ST` for global managed data
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataSymbol {
    /// Whether this data is global or local.
    pub global: bool,
    /// Whether this data is managed or unmanaged.
    pub managed: bool,
    /// Type identifier of the type of data.
    pub type_index: TypeIndex,
    /// Code offset of the start of the data region.
    pub offset: PdbInternalSectionOffset,
    /// Name of the data variable.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for DataSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = DataSymbol {
            global: matches!(kind, S_GDATA32 | S_GDATA32_ST | S_GMANDATA | S_GMANDATA_ST),
            managed: matches!(
                kind,
                S_LMANDATA | S_LMANDATA_ST | S_GMANDATA | S_GMANDATA_ST
            ),
            type_index: buf.parse()?,
            offset: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// Reference to an imported procedure.
///
/// Symbol kind `S_PROCREF`, `S_PROCREF_ST`, `S_LPROCREF`, or `S_LPROCREF_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcedureReferenceSymbol {
    /// Whether the referenced procedure is global or local.
    pub global: bool,
    /// SUC of the name.
    pub sum_name: u32,
    /// Symbol index of the referenced [`ProcedureSymbol`].
    ///
    /// Note that this symbol might be located in a different module.
    pub symbol_index: SymbolIndex,
    /// Index of the module in [`DebugInformation::modules`](crate::DebugInformation::modules)
    /// containing the actual symbol.
    pub module: Option<usize>,
    /// Name of the procedure reference.
    pub name: Option<String>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for ProcedureReferenceSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let global = matches!(kind, S_PROCREF | S_PROCREF_ST);
        let sum_name = buf.parse()?;
        let symbol_index = buf.parse()?;
        // 1-based module index in the input - presumably 0 means invalid / not present
        let module = buf.parse::<u16>()?.checked_sub(1).map(usize::from);
        let name = parse_optional_name(&mut buf, kind)?;

        let symbol = ProcedureReferenceSymbol {
            global,
            sum_name,
            symbol_index,
            module,
            name: name.map(|x| x.to_string().to_string()),
        };

        Ok((symbol, buf.pos()))
    }
}

/// Reference to an imported variable.
///
/// Symbol kind `S_DATAREF`, or `S_DATAREF_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataReferenceSymbol {
    /// SUC of the name.
    pub sum_name: u32,
    /// Symbol index of the referenced [`DataSymbol`].
    ///
    /// Note that this symbol might be located in a different module.
    pub symbol_index: SymbolIndex,
    /// Index of the module in [`DebugInformation::modules`](crate::DebugInformation::modules)
    /// containing the actual symbol.
    pub module: Option<usize>,
    /// Name of the data reference.
    pub name: Option<String>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for DataReferenceSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let sum_name = buf.parse()?;
        let symbol_index = buf.parse()?;
        // 1-based module index in the input - presumably 0 means invalid / not present
        let module = buf.parse::<u16>()?.checked_sub(1).map(usize::from);
        let name = parse_optional_name(&mut buf, kind)?;

        let symbol = DataReferenceSymbol {
            sum_name,
            symbol_index,
            module,
            name: name.map(|x| x.to_string().to_string()),
        };

        Ok((symbol, buf.pos()))
    }
}

/// Reference to an annotation.
///
/// Symbol kind `S_ANNOTATIONREF`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnotationReferenceSymbol {
    /// SUC of the name.
    pub sum_name: u32,
    /// Symbol index of the referenced symbol.
    ///
    /// Note that this symbol might be located in a different module.
    pub symbol_index: SymbolIndex,
    /// Index of the module in [`DebugInformation::modules`](crate::DebugInformation::modules)
    /// containing the actual symbol.
    pub module: Option<usize>,
    /// Name of the annotation reference.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for AnnotationReferenceSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let sum_name = buf.parse()?;
        let symbol_index = buf.parse()?;
        // 1-based module index in the input - presumably 0 means invalid / not present
        let module = buf.parse::<u16>()?.checked_sub(1).map(usize::from);
        let name = parse_symbol_name(&mut buf, kind)?.to_string().to_string();

        let symbol = AnnotationReferenceSymbol {
            sum_name,
            symbol_index,
            module,
            name,
        };

        Ok((symbol, buf.pos()))
    }
}

/// Reference to a managed procedure symbol (`S_LMANPROC` or `S_GMANPROC`).
///
/// Symbol kind `S_TOKENREF`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenReferenceSymbol {
    /// SUC of the name.
    pub sum_name: u32,
    /// Symbol index of the referenced [`ManagedProcedureSymbol`].
    ///
    /// Note that this symbol might be located in a different module.
    pub symbol_index: SymbolIndex,
    /// Index of the module in [`DebugInformation::modules`](crate::DebugInformation::modules)
    /// containing the actual symbol.
    pub module: Option<usize>,
    /// Name of the procedure reference.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for TokenReferenceSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let sum_name = buf.parse()?;
        let symbol_index = buf.parse()?;
        // 1-based module index in the input - presumably 0 means invalid / not present
        let module = buf.parse::<u16>()?.checked_sub(1).map(usize::from);
        let name = parse_symbol_name(&mut buf, kind)?.to_string().to_string();

        let symbol = TokenReferenceSymbol {
            sum_name,
            symbol_index,
            module,
            name,
        };

        Ok((symbol, buf.pos()))
    }
}

/// Subtype of [`TrampolineSymbol`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrampolineType {
    /// An incremental thunk.
    Incremental,
    /// Branch island thunk.
    BranchIsland,
    /// An unknown thunk type.
    Unknown,
}

/// Trampoline thunk.
///
/// Symbol kind `S_TRAMPOLINE`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrampolineSymbol {
    /// Trampoline symbol subtype.
    pub tramp_type: TrampolineType,
    /// Code size of the thunk.
    pub size: u16,
    /// Code offset of the thunk.
    pub thunk: PdbInternalSectionOffset,
    /// Code offset of the thunk target.
    pub target: PdbInternalSectionOffset,
}

impl TryFromCtx<'_, SymbolKind> for TrampolineSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'_ [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let tramp_type = match buf.parse::<u16>()? {
            0x00 => TrampolineType::Incremental,
            0x01 => TrampolineType::BranchIsland,
            _ => TrampolineType::Unknown,
        };

        let size = buf.parse()?;
        let thunk_offset = buf.parse()?;
        let target_offset = buf.parse()?;
        let thunk_section = buf.parse()?;
        let target_section = buf.parse()?;

        let symbol = Self {
            tramp_type,
            size,
            thunk: PdbInternalSectionOffset::new(thunk_section, thunk_offset),
            target: PdbInternalSectionOffset::new(target_section, target_offset),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A constant value.
///
/// Symbol kind `S_CONSTANT`, or `S_CONSTANT_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConstantSymbol {
    /// Whether this constant has metadata type information.
    pub managed: bool,
    /// The type of this constant or metadata token.
    pub type_index: TypeIndex,
    /// The value of this constant.
    pub value: Variant,
    /// Name of the constant.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for ConstantSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = ConstantSymbol {
            managed: kind == S_MANCONSTANT,
            type_index: buf.parse()?,
            value: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A user defined type.
///
/// Symbol kind `S_UDT`, or `S_UDT_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserDefinedTypeSymbol {
    /// Identifier of the type.
    pub type_index: TypeIndex,
    /// Name of the type.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for UserDefinedTypeSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = UserDefinedTypeSymbol {
            type_index: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A thread local variable.
///
/// Symbol kinds:
///  - `S_LTHREAD32`, `S_LTHREAD32_ST` for local thread storage.
///  - `S_GTHREAD32`, or `S_GTHREAD32_ST` for global thread storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadStorageSymbol {
    /// Whether this is a global or local thread storage.
    pub global: bool,
    /// Identifier of the stored type.
    pub type_index: TypeIndex,
    /// Code offset of the thread local.
    pub offset: PdbInternalSectionOffset,
    /// Name of the thread local.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for ThreadStorageSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = ThreadStorageSymbol {
            global: matches!(kind, S_GTHREAD32 | S_GTHREAD32_ST),
            type_index: buf.parse()?,
            offset: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

// CV_PROCFLAGS:
const CV_PFLAG_NOFPO: u8 = 0x01;
const CV_PFLAG_INT: u8 = 0x02;
const CV_PFLAG_FAR: u8 = 0x04;
const CV_PFLAG_NEVER: u8 = 0x08;
const CV_PFLAG_NOTREACHED: u8 = 0x10;
const CV_PFLAG_CUST_CALL: u8 = 0x20;
const CV_PFLAG_NOINLINE: u8 = 0x40;
const CV_PFLAG_OPTDBGINFO: u8 = 0x80;

/// Flags of a [`ProcedureSymbol`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcedureFlags {
    /// Frame pointer is present (not omitted).
    pub nofpo: bool,
    /// Interrupt return.
    pub int: bool,
    /// Far return.
    pub far: bool,
    /// Procedure does not return.
    pub never: bool,
    /// Procedure is never called.
    pub notreached: bool,
    /// Custom calling convention.
    pub cust_call: bool,
    /// Marked as `noinline`.
    pub noinline: bool,
    /// Debug information for optimized code is present.
    pub optdbginfo: bool,
}

impl<'t> TryFromCtx<'t, Endian> for ProcedureFlags {
    type Error = scroll::Error;

    fn try_from_ctx(this: &'t [u8], le: Endian) -> scroll::Result<(Self, usize)> {
        let (value, size) = u8::try_from_ctx(this, le)?;

        let flags = Self {
            nofpo: value & CV_PFLAG_NOFPO != 0,
            int: value & CV_PFLAG_INT != 0,
            far: value & CV_PFLAG_FAR != 0,
            never: value & CV_PFLAG_NEVER != 0,
            notreached: value & CV_PFLAG_NOTREACHED != 0,
            cust_call: value & CV_PFLAG_CUST_CALL != 0,
            noinline: value & CV_PFLAG_NOINLINE != 0,
            optdbginfo: value & CV_PFLAG_OPTDBGINFO != 0,
        };

        Ok((flags, size))
    }
}

/// A procedure, such as a function or method.
///
/// Symbol kinds:
///  - `S_GPROC32`, `S_GPROC32_ST` for global procedures
///  - `S_LPROC32`, `S_LPROC32_ST` for local procedures
///  - `S_LPROC32_DPC` for DPC procedures
///  - `S_GPROC32_ID`, `S_LPROC32_ID`, `S_LPROC32_DPC_ID` for procedures referencing types from the
///    ID stream rather than the Type stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcedureSymbol {
    /// Whether this is a global or local procedure.
    pub global: bool,
    /// Indicates Deferred Procedure Calls (DPC).
    pub dpc: bool,
    /// The parent scope that this procedure is nested in.
    pub parent: Option<SymbolIndex>,
    /// The end symbol of this procedure.
    pub end: SymbolIndex,
    /// The next procedure symbol.
    pub next: Option<SymbolIndex>,
    /// The length of the code block covered by this procedure.
    pub len: u32,
    /// Start offset of the procedure's body code, which marks the end of the prologue.
    pub dbg_start_offset: u32,
    /// End offset of the procedure's body code, which marks the start of the epilogue.
    pub dbg_end_offset: u32,
    /// Identifier of the procedure type.
    ///
    /// The type contains the complete signature, including parameters, modifiers and the return
    /// type.
    pub type_index: TypeIndex,
    /// Code offset of the start of this procedure.
    pub offset: PdbInternalSectionOffset,
    /// Detailed flags of this procedure.
    pub flags: ProcedureFlags,
    /// The full, demangled name of the procedure.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for ProcedureSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = ProcedureSymbol {
            global: matches!(kind, S_GPROC32 | S_GPROC32_ST | S_GPROC32_ID),
            dpc: matches!(kind, S_LPROC32_DPC | S_LPROC32_DPC_ID),
            parent: parse_optional_index(&mut buf)?,
            end: buf.parse()?,
            next: parse_optional_index(&mut buf)?,
            len: buf.parse()?,
            dbg_start_offset: buf.parse()?,
            dbg_end_offset: buf.parse()?,
            type_index: buf.parse()?,
            offset: buf.parse()?,
            flags: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A managed procedure, such as a function or method.
///
/// Symbol kinds:
/// - `S_GMANPROC`, `S_GMANPROCIA64` for global procedures
/// - `S_LMANPROC`, `S_LMANPROCIA64` for local procedures
///
/// `S_GMANPROCIA64` and `S_LMANPROCIA64` are only mentioned, there is no available source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedProcedureSymbol {
    /// Whether this is a global or local procedure.
    pub global: bool,
    /// The parent scope that this procedure is nested in.
    pub parent: Option<SymbolIndex>,
    /// The end symbol of this procedure.
    pub end: SymbolIndex,
    /// The next procedure symbol.
    pub next: Option<SymbolIndex>,
    /// The length of the code block covered by this procedure.
    pub len: u32,
    /// Start offset of the procedure's body code, which marks the end of the prologue.
    pub dbg_start_offset: u32,
    /// End offset of the procedure's body code, which marks the start of the epilogue.
    pub dbg_end_offset: u32,
    /// COM+ metadata token
    pub token: COMToken,
    /// Code offset of the start of this procedure.
    pub offset: PdbInternalSectionOffset,
    /// Detailed flags of this procedure.
    pub flags: ProcedureFlags,
    /// Register return value is in (may not be used for all archs).
    pub return_register: u16,
    /// Optional name of the procedure.
    pub name: Option<String>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for ManagedProcedureSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = ManagedProcedureSymbol {
            global: matches!(kind, S_GMANPROC),
            parent: parse_optional_index(&mut buf)?,
            end: buf.parse()?,
            next: parse_optional_index(&mut buf)?,
            len: buf.parse()?,
            dbg_start_offset: buf.parse()?,
            dbg_end_offset: buf.parse()?,
            token: buf.parse()?,
            offset: buf.parse()?,
            flags: buf.parse()?,
            return_register: buf.parse()?,
            name: parse_optional_name(&mut buf, kind)?.map(|x| x.to_string().to_string()),
        };

        Ok((symbol, buf.pos()))
    }
}

/// The callsite of an inlined function.
///
/// Symbol kind `S_INLINESITE`, or `S_INLINESITE2`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineSiteSymbol {
    /// Index of the parent function.
    ///
    /// This might either be a [`ProcedureSymbol`] or another `InlineSiteSymbol`.
    pub parent: Option<SymbolIndex>,
    /// The end symbol of this callsite.
    pub end: SymbolIndex,
    /// Identifier of the type describing the inline function.
    pub inlinee: IdIndex,
    /// The total number of invocations of the inline function.
    pub invocations: Option<u32>,
    /// Binary annotations containing the line program of this call site.
    pub annotations: BinaryAnnotations,
}

impl<'t> TryFromCtx<'t, SymbolKind> for InlineSiteSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = InlineSiteSymbol {
            parent: parse_optional_index(&mut buf)?,
            end: buf.parse()?,
            inlinee: buf.parse()?,
            invocations: match kind {
                S_INLINESITE2 => Some(buf.parse()?),
                _ => None,
            },
            annotations: BinaryAnnotations::new(buf.take(buf.len())?),
        };

        Ok((symbol, buf.pos()))
    }
}

/// Reference to build information.
///
/// Symbol kind `S_BUILDINFO`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildInfoSymbol {
    /// Index of the build information record.
    pub id: IdIndex,
}

impl<'t> TryFromCtx<'t, SymbolKind> for BuildInfoSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = Self { id: buf.parse()? };

        Ok((symbol, buf.pos()))
    }
}

/// Name of the object file of this module.
///
/// Symbol kind `S_OBJNAME`, or `S_OBJNAME_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjNameSymbol {
    /// Signature.
    pub signature: u32,
    /// Path to the object file.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for ObjNameSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = ObjNameSymbol {
            signature: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A version number refered to by `CompileFlagsSymbol`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerVersion {
    /// The major version number.
    pub major: u16,
    /// The minor version number.
    pub minor: u16,
    /// The build (patch) version number.
    pub build: u16,
    /// The QFE (quick fix engineering) number.
    pub qfe: Option<u16>,
}

impl<'t> TryFromCtx<'t, bool> for CompilerVersion {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], has_qfe: bool) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let version = Self {
            major: buf.parse()?,
            minor: buf.parse()?,
            build: buf.parse()?,
            qfe: if has_qfe { Some(buf.parse()?) } else { None },
        };

        Ok((version, buf.pos()))
    }
}

/// Compile flags declared in `CompileFlagsSymbol`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompileFlags {
    /// Compiled for edit and continue.
    pub edit_and_continue: bool,
    /// Compiled without debugging info.
    pub no_debug_info: bool,
    /// Compiled with `LTCG`.
    pub link_time_codegen: bool,
    /// Compiled with `/bzalign`.
    pub no_data_align: bool,
    /// Managed code or data is present.
    pub managed: bool,
    /// Compiled with `/GS`.
    pub security_checks: bool,
    /// Compiled with `/hotpatch`.
    pub hot_patch: bool,
    /// Compiled with `CvtCIL`.
    pub cvtcil: bool,
    /// This is a MSIL .NET Module.
    pub msil_module: bool,
    /// Compiled with `/sdl`.
    pub sdl: bool,
    /// Compiled with `/ltcg:pgo` or `pgo:`.
    pub pgo: bool,
    /// This is a .exp module.
    pub exp_module: bool,
}

impl<'t> TryFromCtx<'t, SymbolKind> for CompileFlags {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let is_compile3 = kind == S_COMPILE3;

        let raw = this.pread_with::<u16>(0, LE)?;
        this.pread::<u8>(2)?; // unused

        let flags = Self {
            edit_and_continue: raw & 1 != 0,
            no_debug_info: (raw >> 1) & 1 != 0,
            link_time_codegen: (raw >> 2) & 1 != 0,
            no_data_align: (raw >> 3) & 1 != 0,
            managed: (raw >> 4) & 1 != 0,
            security_checks: (raw >> 5) & 1 != 0,
            hot_patch: (raw >> 6) & 1 != 0,
            cvtcil: (raw >> 7) & 1 != 0,
            msil_module: (raw >> 8) & 1 != 0,
            sdl: (raw >> 9) & 1 != 0 && is_compile3,
            pgo: (raw >> 10) & 1 != 0 && is_compile3,
            exp_module: (raw >> 11) & 1 != 0 && is_compile3,
        };

        Ok((flags, 3))
    }
}

/// Flags used to compile a module.
///
/// Symbol kind `S_COMPILE2`, `S_COMPILE2_ST`, or `S_COMPILE3`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompileFlagsSymbol {
    /// The source code language.
    pub language: SourceLanguage,
    /// Compiler flags.
    pub flags: CompileFlags,
    /// Machine type of the compilation target.
    pub cpu_type: CPUType,
    /// Version of the compiler frontend.
    pub frontend_version: CompilerVersion,
    /// Version of the compiler backend.
    pub backend_version: CompilerVersion,
    /// Display name of the compiler.
    pub version_string: String,
    // TODO: Command block for S_COMPILE2?
}

impl<'t> TryFromCtx<'t, SymbolKind> for CompileFlagsSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let has_qfe = kind == S_COMPILE3;
        let symbol = CompileFlagsSymbol {
            language: buf.parse()?,
            flags: buf.parse_with(kind)?,
            cpu_type: buf.parse()?,
            frontend_version: buf.parse_with(has_qfe)?,
            backend_version: buf.parse_with(has_qfe)?,
            version_string: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A using namespace directive.
///
/// Symbol kind `S_UNAMESPACE`, or `S_UNAMESPACE_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsingNamespaceSymbol {
    /// The name of the imported namespace.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for UsingNamespaceSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = UsingNamespaceSymbol {
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

// CV_LVARFLAGS:
const CV_LVARFLAG_ISPARAM: u16 = 0x01;
const CV_LVARFLAG_ADDRTAKEN: u16 = 0x02;
const CV_LVARFLAG_COMPGENX: u16 = 0x04;
const CV_LVARFLAG_ISAGGREGATE: u16 = 0x08;
const CV_LVARFLAG_ISALIASED: u16 = 0x10;
const CV_LVARFLAG_ISALIAS: u16 = 0x20;
const CV_LVARFLAG_ISRETVALUE: u16 = 0x40;
const CV_LVARFLAG_ISOPTIMIZEDOUT: u16 = 0x80;
const CV_LVARFLAG_ISENREG_GLOB: u16 = 0x100;
const CV_LVARFLAG_ISENREG_STAT: u16 = 0x200;

/// Flags for a [`LocalSymbol`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalVariableFlags {
    /// Variable is a parameter.
    pub isparam: bool,
    /// Address is taken.
    pub addrtaken: bool,
    /// Variable is compiler generated.
    pub compgenx: bool,
    /// The symbol is splitted in temporaries, which are treated by compiler as independent
    /// entities.
    pub isaggregate: bool,
    /// Variable has multiple simultaneous lifetimes.
    pub isaliased: bool,
    /// Represents one of the multiple simultaneous lifetimes.
    pub isalias: bool,
    /// Represents a function return value.
    pub isretvalue: bool,
    /// Variable has no lifetimes.
    pub isoptimizedout: bool,
    /// Variable is an enregistered global.
    pub isenreg_glob: bool,
    /// Variable is an enregistered static.
    pub isenreg_stat: bool,
}

impl<'t> TryFromCtx<'t, Endian> for LocalVariableFlags {
    type Error = scroll::Error;

    fn try_from_ctx(this: &'t [u8], le: Endian) -> scroll::Result<(Self, usize)> {
        let (value, size) = u16::try_from_ctx(this, le)?;

        let flags = Self {
            isparam: value & CV_LVARFLAG_ISPARAM != 0,
            addrtaken: value & CV_LVARFLAG_ADDRTAKEN != 0,
            compgenx: value & CV_LVARFLAG_COMPGENX != 0,
            isaggregate: value & CV_LVARFLAG_ISAGGREGATE != 0,
            isaliased: value & CV_LVARFLAG_ISALIASED != 0,
            isalias: value & CV_LVARFLAG_ISALIAS != 0,
            isretvalue: value & CV_LVARFLAG_ISRETVALUE != 0,
            isoptimizedout: value & CV_LVARFLAG_ISOPTIMIZEDOUT != 0,
            isenreg_glob: value & CV_LVARFLAG_ISENREG_GLOB != 0,
            isenreg_stat: value & CV_LVARFLAG_ISENREG_STAT != 0,
        };

        Ok((flags, size))
    }
}

/// A local symbol in optimized code.
///
/// Symbol kind `S_LOCAL`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalSymbol {
    /// The type of the symbol.
    pub type_index: TypeIndex,
    /// Flags for this symbol.
    pub flags: LocalVariableFlags,
    /// Name of the symbol.
    pub name: String,
    /// Parameter slot
    pub slot: Option<i32>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for LocalSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let type_index: TypeIndex = buf.parse()?;
        let flags: LocalVariableFlags = buf.parse()?;
        let name: RawString<'t> = parse_symbol_name(&mut buf, kind)?;

        let slot: Option<i32> = if (this.len() as i64 - name.len() as i64 - 8i64) >= 6 {
            if this[name.len() + 0xb] == 0x24 {
                Some(ParseBuffer::from(&this[(name.len() + 0xc)..]).parse()?)
            } else {
                None
            }
        } else {
            None
        };

        Ok((
            Self {
                type_index,
                flags,
                name: name.to_string().to_string(),
                slot,
            },
            buf.pos(),
        ))
    }
}

/// A managed local variable slot.
///
/// Symbol kind `S_MANSLOT`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedSlotSymbol {
    /// Slot index.
    pub slot: u32,
    /// Type index or metadata token.
    pub type_index: TypeIndex,
    /// First code address where var is live.
    pub offset: PdbInternalSectionOffset,
    /// Local variable flags.
    pub flags: LocalVariableFlags,
    /// Length-prefixed name of the variable.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for ManagedSlotSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = ManagedSlotSymbol {
            slot: buf.parse()?,
            type_index: buf.parse()?,
            offset: buf.parse()?,
            flags: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L3102
/// An address range of a live range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddressRange {
    /// Offset of the range.
    pub offset: PdbInternalSectionOffset,
    /// Length of the range.
    pub cb_range: u16,
}

impl<'t> TryFromCtx<'t, Endian> for AddressRange {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], _le: Endian) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let range = Self {
            offset: buf.parse()?,
            cb_range: buf.parse()?,
        };

        Ok((range, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4456
/// Flags of an [`ExportSymbol`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportSymbolFlags {
    /// An exported constant.
    pub constant: bool,
    /// Exported data (e.g. a static variable).
    pub data: bool,
    /// A private symbol.
    pub private: bool,
    /// A symbol with no name.
    pub no_name: bool,
    /// Ordinal was explicitly assigned.
    pub ordinal: bool,
    /// This is a forwarder.
    pub forwarder: bool,
}

impl<'t> TryFromCtx<'t, Endian> for ExportSymbolFlags {
    type Error = scroll::Error;

    fn try_from_ctx(this: &'t [u8], le: Endian) -> scroll::Result<(Self, usize)> {
        let (value, size) = u16::try_from_ctx(this, le)?;

        let flags = Self {
            constant: value & 0x01 != 0,
            data: value & 0x02 != 0,
            private: value & 0x04 != 0,
            no_name: value & 0x08 != 0,
            ordinal: value & 0x10 != 0,
            forwarder: value & 0x20 != 0,
        };

        Ok((flags, size))
    }
}

/// An exported symbol.
///
/// Symbol kind `S_EXPORT`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportSymbol {
    /// Ordinal of the symbol.
    pub ordinal: u16,
    /// Flags declaring the type of the exported symbol.
    pub flags: ExportSymbolFlags,
    /// The name of the exported symbol.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for ExportSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = ExportSymbol {
            ordinal: buf.parse()?,
            flags: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A label symbol.
///
/// Symbol kind `S_LABEL32`, `S_LABEL16`, or `S_LABEL32_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LabelSymbol {
    /// Code offset of the start of this label.
    pub offset: PdbInternalSectionOffset,
    /// Detailed flags of this label.
    pub flags: ProcedureFlags,
    /// Name of the symbol.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for LabelSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = LabelSymbol {
            offset: buf.parse()?,
            flags: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A block symbol.
///
/// Symbol kind `S_BLOCK32`, or `S_BLOCK32_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockSymbol {
    /// The parent scope that this block is nested in.
    pub parent: SymbolIndex,
    /// The end symbol of this block.
    pub end: SymbolIndex,
    /// The length of the block.
    pub len: u32,
    /// Code offset of the start of this label.
    pub offset: PdbInternalSectionOffset,
    /// The block name.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for BlockSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = BlockSymbol {
            parent: buf.parse()?,
            end: buf.parse()?,
            len: buf.parse()?,
            offset: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A register relative symbol.
///
/// The address of the variable is the value in the register + offset (e.g. %EBP + 8).
///
/// Symbol kind `S_REGREL32`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterRelativeSymbol {
    /// The variable offset.
    pub offset: i32,
    /// The type of the variable.
    pub type_index: TypeIndex,
    /// The register this variable address is relative to.
    pub register: Register,
    /// The variable name.
    pub name: String,
    /// Parameter slot
    pub slot: Option<i32>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for RegisterRelativeSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let offset: i32 = buf.parse()?;
        let type_index: TypeIndex = buf.parse()?;
        let register: Register = buf.parse()?;
        let name: RawString<'t> = parse_symbol_name(&mut buf, kind)?;

        let slot: Option<i32> = if (this.len() as i64 - name.len() as i64 - 0xci64) >= 6 {
            if this[name.len() + 0xf] == 0x24 {
                Some(ParseBuffer::from(&this[(name.len() + 0x10)..]).parse()?)
            } else {
                None
            }
        } else {
            None
        };

        Ok((
            Self {
                offset,
                type_index,
                register,
                name: name.to_string().to_string(),
                slot,
            },
            buf.pos(),
        ))
    }
}

/// Thunk adjustor
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThunkAdjustor {
    delta: u16,
    target: String,
}

/// A thunk kind
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThunkKind {
    /// Standard thunk
    NoType,
    /// "this" adjustor thunk with delta and target
    Adjustor(ThunkAdjustor),
    /// Virtual call thunk with table entry
    VCall(u16),
    /// pcode thunk
    PCode,
    /// thunk which loads the address to jump to via unknown means...
    Load,
    /// Unknown with ordinal value
    Unknown(u8),
}

/// A thunk symbol.
///
/// Symbol kind `S_THUNK32`, or `S_THUNK32_ST`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThunkSymbol {
    /// The parent scope that this thunk is nested in.
    pub parent: Option<SymbolIndex>,
    /// The end symbol of this thunk.
    pub end: SymbolIndex,
    /// The next symbol.
    pub next: Option<SymbolIndex>,
    /// Code offset of the start of this label.
    pub offset: PdbInternalSectionOffset,
    /// The length of the thunk.
    pub len: u16,
    /// The kind of the thunk.
    pub kind: ThunkKind,
    /// The thunk name.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for ThunkSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let parent = parse_optional_index(&mut buf)?;
        let end = buf.parse()?;
        let next = parse_optional_index(&mut buf)?;
        let offset = buf.parse()?;
        let len = buf.parse()?;
        let ord = buf.parse::<u8>()?;
        let name = parse_symbol_name(&mut buf, kind)?.to_string().to_string();

        let kind = match ord {
            0 => ThunkKind::NoType,
            1 => ThunkKind::Adjustor(ThunkAdjustor {
                delta: buf.parse::<u16>()?,
                target: buf.parse_cstring()?.to_string().to_string(),
            }),
            2 => ThunkKind::VCall(buf.parse::<u16>()?),
            3 => ThunkKind::PCode,
            4 => ThunkKind::Load,
            ord => ThunkKind::Unknown(ord),
        };

        let symbol = ThunkSymbol {
            parent,
            end,
            next,
            offset,
            len,
            kind,
            name,
        };

        Ok((symbol, buf.pos()))
    }
}

// CV_SEPCODEFLAGS:
const CV_SEPCODEFLAG_IS_LEXICAL_SCOPE: u32 = 0x01;
const CV_SEPCODEFLAG_RETURNS_TO_PARENT: u32 = 0x02;

/// Flags for a [`SeparatedCodeSymbol`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeparatedCodeFlags {
    /// `S_SEPCODE` doubles as lexical scope.
    pub islexicalscope: bool,
    /// code frag returns to parent.
    pub returnstoparent: bool,
}

impl<'t> TryFromCtx<'t, Endian> for SeparatedCodeFlags {
    type Error = scroll::Error;

    fn try_from_ctx(this: &'t [u8], le: Endian) -> scroll::Result<(Self, usize)> {
        let (value, size) = u32::try_from_ctx(this, le)?;

        let flags = Self {
            islexicalscope: value & CV_SEPCODEFLAG_IS_LEXICAL_SCOPE != 0,
            returnstoparent: value & CV_SEPCODEFLAG_RETURNS_TO_PARENT != 0,
        };

        Ok((flags, size))
    }
}

/// A separated code symbol.
///
/// Symbol kind `S_SEPCODE`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeparatedCodeSymbol {
    /// The parent scope that this block is nested in.
    pub parent: SymbolIndex,
    /// The end symbol of this block.
    pub end: SymbolIndex,
    /// The length of the block.
    pub len: u32,
    /// Flags for this symbol
    pub flags: SeparatedCodeFlags,
    /// Code offset of the start of the separated code.
    pub offset: PdbInternalSectionOffset,
    /// Parent offset.
    pub parent_offset: PdbInternalSectionOffset,
}

impl<'t> TryFromCtx<'t, SymbolKind> for SeparatedCodeSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], _: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let parent = buf.parse()?;
        let end = buf.parse()?;
        let len = buf.parse()?;
        let flags = buf.parse()?;
        let offset = buf.parse()?;
        let parent_offset = buf.parse()?;
        let section = buf.parse()?;
        let parent_section = buf.parse()?;

        let symbol = Self {
            parent,
            end,
            len,
            flags,
            offset: PdbInternalSectionOffset { offset, section },
            parent_offset: PdbInternalSectionOffset {
                offset: parent_offset,
                section: parent_section,
            },
        };

        Ok((symbol, buf.pos()))
    }
}

/// An OEM symbol.
///
/// Symbol kind `S_OEM`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OemSymbol {
    /// OEM's identifier (16B GUID).
    pub id_oem: String,
    /// Type index.
    pub type_index: TypeIndex,
    /// User data with forced 4B-alignment.
    ///
    /// An array of variable size, currently only the first 4B are parsed.
    pub rgl: u32,
}

impl<'t> TryFromCtx<'t, SymbolKind> for OemSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = OemSymbol {
            id_oem: buf.parse_cstring()?.to_string().to_string(),
            type_index: buf.parse()?,
            rgl: buf.parse()?,
        };

        Ok((symbol, buf.pos()))
    }
}

/// Environment block split off from `S_COMPILE2`.
///
/// Symbol kind `S_ENVBLOCK`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvBlockSymbol {
    /// EC flag (previously called `rev`).
    pub edit_and_continue: bool,
    /// Sequence of zero-terminated command strings.
    pub rgsz: Vec<String>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for EnvBlockSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);
        let flags: u8 = buf.parse()?;

        let mut strings = Vec::new();

        while !buf.is_empty() {
            strings.push(parse_symbol_name(&mut buf, kind)?.to_string().to_string());
        }

        let symbol = EnvBlockSymbol {
            edit_and_continue: flags & 1 != 0,
            rgsz: strings,
        };

        Ok((symbol, buf.pos()))
    }
}

/// A COFF section in a PE executable.
///
/// Symbol kind `S_SECTION`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionSymbol {
    /// Section number.
    pub isec: u16,
    ///  Alignment of this section (power of 2).
    pub align: u8,
    /// Reserved.  Must be zero.
    pub reserved: u8,
    /// Section's RVA.
    pub rva: u32,
    /// Section's CB.
    pub cb: u32,
    /// Section characteristics.
    pub characteristics: SectionCharacteristics,
    /// Section name.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for SectionSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = SectionSymbol {
            isec: buf.parse()?,
            align: buf.parse()?,
            reserved: buf.parse()?,
            rva: buf.parse()?,
            cb: buf.parse()?,
            characteristics: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

/// A COFF section in a PE executable.
///
/// Symbol kind `S_COFFGROUP`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoffGroupSymbol {
    /// COFF group's CB.
    pub cb: u32,
    /// COFF group characteristics.
    pub characteristics: u32,
    /// Symbol offset.
    pub offset: PdbInternalSectionOffset,
    /// COFF group name.
    pub name: String,
}

impl<'t> TryFromCtx<'t, SymbolKind> for CoffGroupSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = CoffGroupSymbol {
            cb: buf.parse()?,
            characteristics: buf.parse()?,
            offset: buf.parse()?,
            name: parse_symbol_name(&mut buf, kind)?.to_string().to_string(),
        };

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L3111
/// A gap in a live range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddressGap {
    /// Relative offset from the beginning of the live range
    pub gap_start_offset: u16,
    /// Length of the gap
    pub cb_range: u16,
}

impl<'t> TryFromCtx<'t, Endian> for AddressGap {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], _: Endian) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let range = Self {
            gap_start_offset: buf.parse()?,
            cb_range: buf.parse()?,
        };

        Ok((range, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4209
/// A live range of sub field of variable
///
/// Symbol kind `S_DEFRANGE`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefRangeSymbol {
    /// DIA program to evaluate the value of the symbol
    pub program: u32,
    /// Range of addresses where this program is valid
    pub range: AddressRange,
    /// The value is not available in following gaps
    pub gaps: Vec<AddressGap>,
}

impl TryFromCtx<'_, SymbolKind> for DefRangeSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'_ [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        // https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4313
        let gap_count = (
            buf.len() + 4 /* sizeof(reclen) + buf offset */
                - 16 /* sizeof(DEFRANGESYM) */
        ) / 4 /* sizeof(CV_LVAR_ADDR_GAP) */;
        let mut symbol = Self {
            program: buf.parse()?,
            range: buf.parse()?,
            gaps: vec![],
        };
        for _ in 0..gap_count {
            symbol.gaps.push(buf.parse()?);
        }

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L3102
/// A live range of sub field of variable. like locala.i
///
/// Symbol kind `S_DEFRANGE_SUBFIELD`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefRangeSubFieldSymbol {
    /// DIA program to evaluate the value of the symbol
    pub program: u32,
    /// Offset in parent variable.
    pub parent_offset: u32,
    /// Range of addresses where this program is valid
    pub range: AddressRange,
    /// The value is not available in following gaps
    pub gaps: Vec<AddressGap>,
}

impl TryFromCtx<'_, SymbolKind> for DefRangeSubFieldSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'_ [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        // https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4313
        let gap_count = (
            buf.len() + 4 /* sizeof(reclen) + buf offset */
                - 20 /* sizeof(DEFRANGESYMSUBFIELD) */
        ) / 4 /* sizeof(CV_LVAR_ADDR_GAP) */;
        let mut symbol = Self {
            program: buf.parse()?,
            parent_offset: buf.parse()?,
            range: buf.parse()?,
            gaps: vec![],
        };
        for _ in 0..gap_count {
            symbol.gaps.push(buf.parse()?);
        }

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4231
/// Flags of a [`DefRangeRegisterSymbol`] or [`DefRangeSubFieldRegisterSymbol`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeFlags {
    /// May have no user name on one of control flow path.
    pub maybe: bool,
}

impl<'t> TryFromCtx<'t, Endian> for RangeFlags {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], le: Endian) -> std::result::Result<(Self, usize), Self::Error> {
        let (value, size) = u16::try_from_ctx(this, le)?;

        let flags = Self {
            maybe: value & 0x01 != 0,
        };

        Ok((flags, size))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4236
/// A live range of en-registed variable
///
/// Symbol type `S_DEFRANGE_REGISTER`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefRangeRegisterSymbol {
    /// Register to hold the value of the symbol
    pub register: Register,
    /// Attribute of the register range.
    pub flags: RangeFlags,
    /// Range of addresses where this program is valid
    pub range: AddressRange,
    /// The value is not available in following gaps
    pub gaps: Vec<AddressGap>,
}

impl TryFromCtx<'_, SymbolKind> for DefRangeRegisterSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'_ [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        // https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4313
        let gap_count = (
            buf.len() + 4 /* sizeof(reclen) + buf offset */
                - 16 /* sizeof(DEFRANGESYM) */
        ) / 4 /* sizeof(CV_LVAR_ADDR_GAP) */;
        let mut symbol = Self {
            register: buf.parse()?,
            flags: buf.parse()?,
            range: buf.parse()?,
            gaps: vec![],
        };
        for _ in 0..gap_count {
            symbol.gaps.push(buf.parse()?);
        }

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4245
/// A live range of frame variable
///
/// Symbol type `S_DEFRANGE_FRAMEPOINTER_REL`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefRangeFramePointerRelativeSymbol {
    /// offset to frame pointer
    pub offset: i32,
    /// Range of addresses where this program is valid
    pub range: AddressRange,
    /// The value is not available in following gaps
    pub gaps: Vec<AddressGap>,
}

impl TryFromCtx<'_, SymbolKind> for DefRangeFramePointerRelativeSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'_ [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        // https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4313
        let gap_count = (
            buf.len() + 4 /* sizeof(reclen) + buf offset */
                - 16 /* sizeof(DEFRANGESYM) */
        ) / 4 /* sizeof(CV_LVAR_ADDR_GAP) */;
        let mut symbol = Self {
            offset: buf.parse()?,
            range: buf.parse()?,
            gaps: vec![],
        };
        for _ in 0..gap_count {
            symbol.gaps.push(buf.parse()?);
        }

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4255
/// A frame variable valid in all function scope
///
/// Symbol type `S_DEFRANGE_FRAMEPOINTER_REL_FULL_SCOPE`
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DefRangeFramePointerRelativeFullScopeSymbol {
    /// offset to frame pointer
    pub offset: i32,
}

impl TryFromCtx<'_, SymbolKind> for DefRangeFramePointerRelativeFullScopeSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'_ [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = Self {
            offset: buf.parse()?,
        };

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4264
/// A live range of sub field of variable. like locala.i
///
/// Symbol type `S_DEFRANGE_SUBFIELD_REGISTER`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefRangeSubFieldRegisterSymbol {
    /// Register to hold the value of the symbol
    pub register: Register,
    /// Attribute of the register range.
    pub flags: RangeFlags,
    /// Offset in parent variable.
    pub offset: u32,
    /// Range of addresses where this program is valid
    pub range: AddressRange,
    /// The value is not available in following gaps
    pub gaps: Vec<AddressGap>,
}

impl TryFromCtx<'_, SymbolKind> for DefRangeSubFieldRegisterSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'_ [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        // https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4313
        let gap_count = (
            buf.len() + 4 /* sizeof(reclen) + buf offset */
                - 20 /* sizeof(DEFRANGESYMSUBFIELD) */
        ) / 4 /* sizeof(CV_LVAR_ADDR_GAP) */;

        let register: Register = buf.parse()?;
        let flags: RangeFlags = buf.parse()?;
        let offset_padding: u32 = buf.parse()?;
        let offset = offset_padding & 0xFFFu32;

        let mut symbol = Self {
            register,
            flags,
            offset,
            range: buf.parse()?,
            gaps: vec![],
        };
        for _ in 0..gap_count {
            symbol.gaps.push(buf.parse()?);
        }

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4279
/// A live range of variable related to a register.
///
/// Symbol type `S_DEFRANGE_REGISTER_REL`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefRangeRegisterRelativeSymbol {
    /// Register to hold the base pointer of the symbol
    pub base_register: Register,
    /// Spilled member for s.i.
    pub spilled_udt_member: u16,
    /// Offset in parent variable.
    pub offset_parent: u16,
    /// offset to register
    pub offset_base_pointer: i32,
    /// Range of addresses where this program is valid
    pub range: AddressRange,
    /// The value is not available in following gaps
    pub gaps: Vec<AddressGap>,
}

impl TryFromCtx<'_, SymbolKind> for DefRangeRegisterRelativeSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'_ [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        // https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4313
        let gap_count = (
            buf.len() + 4 /* sizeof(reclen) + buf offset */
                - 20 /* sizeof(DEFRANGESYMSUBFIELD) */
        ) / 4 /* sizeof(CV_LVAR_ADDR_GAP) */;

        let base_register: Register = buf.parse()?;
        let bitfield: u16 = buf.parse()?;
        let spilled_udt_member = bitfield & 0x1;
        let offset_parent = (bitfield >> 4) & 0xFFF;

        let mut symbol = Self {
            base_register,
            spilled_udt_member,
            offset_parent,
            offset_base_pointer: buf.parse()?,
            range: buf.parse()?,
            gaps: vec![],
        };
        for _ in 0..gap_count {
            symbol.gaps.push(buf.parse()?);
        }

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L3573
/// BP-Relative variable
///
/// Symbol type `S_BPREL32`, `S_BPREL32_ST`, `S_BPREL16`, `S_BPREL32_16T`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BasePointerRelativeSymbol {
    /// BP-relative offset
    pub offset: i32,
    /// Type index or Metadata token
    pub type_index: TypeIndex,
    /// Length-prefixed name
    pub name: String,
    /// Parameter slot
    pub slot: Option<i32>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for BasePointerRelativeSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let offset: i32 = buf.parse()?;
        let type_index = match kind {
            S_BPREL32 | S_BPREL32_ST => buf.parse()?,
            S_BPREL32_16T => TypeIndex::from(buf.parse::<u16>()? as u32),
            _ => return Err(Error::UnimplementedSymbolKind(kind)),
        };
        let name: RawString<'t> = parse_symbol_name(&mut buf, kind)?;

        let slot: Option<i32> = if (this.len() as i64 - name.len() as i64 - 0xai64) >= 6 {
            if this[name.len() + 0xd] == 0x24 {
                Some(ParseBuffer::from(&this[(name.len() + 0xe)..]).parse()?)
            } else {
                None
            }
        } else {
            None
        };

        Ok((
            Self {
                offset,
                type_index,
                name: name.to_string().to_string(),
                slot,
            },
            buf.pos(),
        ))
    }
}

/// Frame procedure flags declared in `FrameProcedureSymbol`
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameProcedureFlags {
    /// function uses `_alloca()`
    has_alloca: bool,
    /// function uses `setjmp()`
    has_setjmp: bool,
    /// function uses `longjmp()`
    has_longjmp: bool,
    /// function uses inline asm
    has_inline_asm: bool,
    /// function has EH states
    has_eh: bool,
    /// function was speced as inline
    inline_spec: bool,
    /// function has `SEH`
    has_seh: bool,
    /// function is `__declspec(naked)`
    naked: bool,
    /// function has buffer security check introduced by `/GS`.
    security_checks: bool,
    /// function compiled with `/EHa`
    async_eh: bool,
    /// function has `/GS` buffer checks, but stack ordering couldn't be done
    gs_no_stack_ordering: bool,
    /// function was inlined within another function
    was_inlined: bool,
    /// function is `__declspec(strict_gs_check)`
    gs_check: bool,
    /// function is `__declspec(safebuffers)`
    safe_buffers: bool,
    /// record function's local pointer explicitly.
    encoded_local_base_pointer: u8,
    /// record function's parameter pointer explicitly.
    encoded_param_base_pointer: u8,
    /// function was compiled with `PGO/PGU`
    pogo_on: bool,
    /// Do we have valid Pogo counts?
    valid_counts: bool,
    /// Did we optimize for speed?
    opt_speed: bool,
    /// function contains CFG checks (and no write checks)
    guard_cf: bool,
    /// function contains CFW checks and/or instrumentation
    guard_cfw: bool,
}

impl<'t> TryFromCtx<'t, Endian> for FrameProcedureFlags {
    type Error = Error;

    fn try_from_ctx(this: &'t [u8], le: Endian) -> Result<(Self, usize)> {
        let raw = this.pread_with::<u32>(0, le)?;
        let flags = Self {
            has_alloca: raw & 1 != 0,
            has_setjmp: (raw >> 1) & 1 != 0,
            has_longjmp: (raw >> 2) & 1 != 0,
            has_inline_asm: (raw >> 3) & 1 != 0,
            has_eh: (raw >> 4) & 1 != 0,
            inline_spec: (raw >> 5) & 1 != 0,
            has_seh: (raw >> 6) & 1 != 0,
            naked: (raw >> 7) & 1 != 0,
            security_checks: (raw >> 8) & 1 != 0,
            async_eh: (raw >> 9) & 1 != 0,
            gs_no_stack_ordering: (raw >> 10) & 1 != 0,
            was_inlined: (raw >> 11) & 1 != 0,
            gs_check: (raw >> 12) & 1 != 0,
            safe_buffers: (raw >> 13) & 1 != 0,
            encoded_local_base_pointer: (raw >> 14) as u8 & 3,
            encoded_param_base_pointer: (raw >> 16) as u8 & 3,
            pogo_on: (raw >> 18) & 1 != 0,
            valid_counts: (raw >> 19) & 1 != 0,
            opt_speed: (raw >> 20) & 1 != 0,
            guard_cf: (raw >> 21) & 1 != 0,
            guard_cfw: (raw >> 22) & 1 != 0,
        };

        Ok((flags, 4))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4069
/// Extra frame and proc information
///
/// Symbol type `S_FRAMEPROC`
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameProcedureSymbol {
    /// count of bytes of total frame of procedure
    pub frame_byte_count: u32,
    /// count of bytes of padding in the frame
    pub padding_byte_count: u32,
    /// offset (relative to frame pointer) to where padding starts
    pub offset_padding: u32,
    /// count of bytes of callee save registers
    pub callee_save_registers_byte_count: u32,
    /// offset of exception handler
    pub exception_handler_offset: PdbInternalSectionOffset,
    /// flags
    pub flags: FrameProcedureFlags,
}

impl TryFromCtx<'_, SymbolKind> for FrameProcedureSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'_ [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let symbol = FrameProcedureSymbol {
            frame_byte_count: buf.parse()?,
            padding_byte_count: buf.parse()?,
            offset_padding: buf.parse()?,
            callee_save_registers_byte_count: buf.parse()?,
            exception_handler_offset: buf.parse()?,
            flags: buf.parse_with(LE)?,
        };

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/Microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4491
/// Indirect call site information
///
/// Symbol type `S_CALLSITEINFO`
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CallSiteInfoSymbol {
    /// offset of call site
    pub offset: PdbInternalSectionOffset,
    /// type index describing function signature
    pub type_index: TypeIndex,
}

impl TryFromCtx<'_, SymbolKind> for CallSiteInfoSymbol {
    type Error = Error;

    fn try_from_ctx(this: &'_ [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let offset: PdbInternalSectionOffset = buf.parse()?;
        let _padding = buf.parse::<u16>()?;
        let type_index: TypeIndex = buf.parse()?;
        let symbol = Self { offset, type_index };

        Ok((symbol, buf.pos()))
    }
}

// https://github.com/microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4382
/// A list of functions and their invocation counts.
///
/// Symbol kind `S_CALLEES` or `S_CALLERS`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionListSymbol {
    /// The list of function indices.
    functions: Vec<TypeIndex>,
    /// The list of invocation counts.
    invocations: Vec<u32>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for FunctionListSymbol {
    type Error = Error;
    fn try_from_ctx(this: &'t [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);
        let count: u32 = buf.parse()?;
        let functions = vec![buf.parse()?; count as usize];

        // the function list is followed by a parallel list of invocation counts.
        // non-existent counts are implicitly zero.
        let mut invocations = Vec::new();
        while !buf.is_empty() {
            invocations.push(buf.parse()?);
        }
        debug_assert!(invocations.len() <= functions.len());
        invocations.resize(functions.len(), 0);

        let symbol = FunctionListSymbol {
            functions,
            invocations,
        };
        Ok((symbol, buf.pos()))
    }
}

// https://github.com/microsoft/microsoft-pdb/issues/50
// LLVM code: https://github.com/llvm/llvm-project/blob/bd92e46204331b9af296f53abb708317e72ab7a8/llvm/lib/DebugInfo/CodeView/TypeIndexDiscovery.cpp#L410
/// List of inlinees of a function
///
/// Symbol kind `S_INLINEES`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineesSymbol {
    /// function ids of the inlinees
    pub inlinees: Vec<TypeIndex>,
}

impl<'t> TryFromCtx<'t, SymbolKind> for InlineesSymbol {
    type Error = Error;
    fn try_from_ctx(this: &'t [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);
        let count = buf.parse::<u32>()?;
        let mut inlinees = Vec::new();
        while !buf.is_empty() {
            inlinees.push(buf.parse()?);
        }
        debug_assert_eq!(inlinees.len(), count as usize);

        let symbol = InlineesSymbol { inlinees };
        Ok((symbol, buf.pos()))
    }
}

/// used to describe the layout of a jump table
///
/// Symbol kind `S_ARMSWITCHTABLE`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArmSwitchTableSymbol {
    /// The base address that the values in the jump table are relative to.
    pub offset_base: PdbInternalSectionOffset,
    /// The type of each entry (absolute pointer, a relative integer, a relative integer that is shifted).
    pub switch_type: JumpTableEntrySize,
    /// The address of the branch instruction that uses the jump table.
    pub offset_branch: PdbInternalSectionOffset,
    /// The address of the jump table.
    pub offset_table: PdbInternalSectionOffset,
    /// The number of entries in the jump table.
    pub num_entries: u32,
}

impl<'t> TryFromCtx<'t, SymbolKind> for ArmSwitchTableSymbol {
    type Error = Error;
    fn try_from_ctx(this: &'t [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let offset_base = buf.parse()?;
        let switch_type = buf.parse()?;
        // need to parse the components of offset_branch and offset_table
        // separately since they are stored in the wrong order
        let off_branch = buf.parse()?;
        let off_table = buf.parse()?;
        let sec_branch = buf.parse()?;
        let sec_table = buf.parse()?;
        let num_entries = buf.parse()?;

        let symbol = ArmSwitchTableSymbol {
            offset_base,
            switch_type,
            offset_branch: PdbInternalSectionOffset {
                offset: off_branch,
                section: sec_branch,
            },
            offset_table: PdbInternalSectionOffset {
                offset: off_table,
                section: sec_table,
            },
            num_entries,
        };
        Ok((symbol, buf.pos()))
    }
}

// https://github.com/microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4366
// enum CV_armswitchtype
/// Enumeration of possible jump table entry sizes.
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum JumpTableEntrySize {
    /// 0x00: Entry type is int8.
    Int8 = 0,
    /// 0x01: Entry type is uint8.
    UInt8 = 1,
    /// 0x02: Entry type is int16.
    Int16 = 2,
    /// 0x03: Entry type is uint16.
    UInt16 = 3,
    /// 0x04: Entry type is int32.
    Int32 = 4,
    /// 0x05: Entry type is uint32.
    UInt32 = 5,
    /// 0x06: Entry type is pointer.
    Pointer = 6,
    /// 0x07: Entry type is uint8 shifted left.
    UInt8ShiftLeft = 7,
    /// 0x08: Entry type is uint16 shifted left.
    UInt16ShiftLeft = 8,
    /// 0x09: Entry type is int8 shifted left.
    Int8ShiftLeft = 9,
    /// 0x0A: Entry type is int16 shifted left.
    Int16ShiftLeft = 10,
    /// 0xFFFF: Invalid entry type, used for error handling.
    Invalid = 0xffff,
}

impl<'t> TryFromCtx<'t, Endian> for JumpTableEntrySize {
    type Error = Error;
    fn try_from_ctx(this: &'t [u8], _unused: Endian) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);
        let value = buf.parse::<u16>()?;
        let size = match value {
            0 => Self::Int8,
            1 => Self::UInt8,
            2 => Self::Int16,
            3 => Self::UInt16,
            4 => Self::Int32,
            5 => Self::UInt32,
            6 => Self::Pointer,
            7 => Self::UInt8ShiftLeft,
            8 => Self::UInt16ShiftLeft,
            9 => Self::Int8ShiftLeft,
            10 => Self::Int16ShiftLeft,
            _ => Self::Invalid,
        };
        Ok((size, buf.pos()))
    }
}

// https://github.com/microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4500
/// Description of a heap allocation site.
///
/// Symbol kind `S_HEAPALLOCSITE`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeapAllocationSiteSymbol {
    /// The offset of the allocation site.
    pub offset: PdbInternalSectionOffset,
    /// length of the heap allocation call instruction
    pub instr_length: u16,
    /// The type index describing the function signature.
    pub type_index: TypeIndex,
}

impl<'t> TryFromCtx<'t, SymbolKind> for HeapAllocationSiteSymbol {
    type Error = Error;
    fn try_from_ctx(this: &'t [u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let offset = buf.parse()?;
        let instr_length = buf.parse()?;
        let type_index = buf.parse()?;

        let symbol = HeapAllocationSiteSymbol {
            offset,
            instr_length,
            type_index,
        };
        Ok((symbol, buf.pos()))
    }
}

// https://github.com/microsoft/microsoft-pdb/blob/082c5290e5aff028ae84e43affa8be717aa7af73/include/cvinfo.h#L4522
/// Description of a security cookie on a stack frame.
///
/// Symbol kind `S_FRAMECOOKIE`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameCookieSymbol {
    /// Frame relative offset
    pub offset: i32,
    /// Register index
    pub register: Register,
    /// Cookie type
    pub cookie_type: FrameCookieType,
    /// Flags
    pub flags: u8, // unknown interpretation
}

impl TryFromCtx<'_, SymbolKind> for FrameCookieSymbol {
    type Error = Error;
    fn try_from_ctx(this: &[u8], _kind: SymbolKind) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);

        let offset = buf.parse()?;
        let register = buf.parse()?;
        let cookie_type = buf.parse()?;
        let flags = buf.parse()?;

        let symbol = FrameCookieSymbol {
            offset,
            register,
            cookie_type,
            flags,
        };
        Ok((symbol, buf.pos()))
    }
}

/// Construction of the security cookie value.
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameCookieType {
    /// Copy
    Copy = 0,
    /// Xor with stack pointer
    XorStackPointer = 1,
    /// Xor with base pointer
    XorBasePointer = 2,
    /// Xor with r13
    XorR13 = 3,
    /// Invalid value - only used for error handling.
    Invalid(u8),
}

impl<'t> TryFromCtx<'t, Endian> for FrameCookieType {
    type Error = Error;
    fn try_from_ctx(this: &'t [u8], _le: Endian) -> Result<(Self, usize)> {
        let mut buf = ParseBuffer::from(this);
        let value = buf.parse::<u8>()?;
        let cookie_type = match value {
            0 => Self::Copy,
            1 => Self::XorStackPointer,
            2 => Self::XorBasePointer,
            3 => Self::XorR13,
            _ => Self::Invalid(value),
        };
        Ok((cookie_type, buf.pos()))
    }
}

/// PDB symbol tables contain names, locations, and metadata about functions, global/static data,
/// constants, data types, and more.
///
/// The `SymbolTable` holds a `SourceView` referencing the symbol table inside the PDB file. All the
/// data structures returned by a `SymbolTable` refer to that buffer.
///
/// # Example
///
/// ```
/// # use pdb2::FallibleIterator;
/// #
/// # fn test() -> pdb2::Result<usize> {
/// let file = std::fs::File::open("fixtures/self/foo.pdb")?;
/// let mut pdb = pdb2::PDB::open(file)?;
///
/// let symbol_table = pdb.global_symbols()?;
/// let address_map = pdb.address_map()?;
///
/// # let mut count: usize = 0;
/// let mut symbols = symbol_table.iter();
/// while let Some(symbol) = symbols.next()? {
///     match symbol.parse() {
///         Ok(pdb2::SymbolData::Public(data)) if data.function => {
///             // we found the location of a function!
///             let rva = data.offset.to_rva(&address_map).unwrap_or_default();
///             println!("{} is {}", rva, data.name);
///             # count += 1;
///         }
///         _ => {}
///     }
/// }
///
/// # Ok(count)
/// # }
/// # assert!(test().expect("test") > 2000);
/// ```
#[derive(Debug)]
pub struct SymbolTable<'s> {
    stream: Stream<'s>,
}

impl<'s> SymbolTable<'s> {
    /// Parses a symbol table from raw stream data.
    #[must_use]
    pub(crate) fn new(stream: Stream<'s>) -> Self {
        SymbolTable { stream }
    }

    /// Returns an iterator that can traverse the symbol table in sequential order.
    #[must_use]
    pub fn iter(&self) -> SymbolIter<'_> {
        SymbolIter::new(self.stream.parse_buffer())
    }

    /// Returns an iterator over symbols starting at the given index.
    #[must_use]
    pub fn iter_at(&self, index: SymbolIndex) -> SymbolIter<'_> {
        let mut iter = self.iter();
        iter.seek(index);
        iter
    }
}

/// A `SymbolIter` iterates over a `SymbolTable`, producing `Symbol`s.
///
/// Symbol tables are represented internally as a series of records, each of which have a length, a
/// type, and a type-specific field layout. Iteration performance is therefore similar to a linked
/// list.
#[derive(Debug)]
pub struct SymbolIter<'t> {
    buf: ParseBuffer<'t>,
}

impl<'t> SymbolIter<'t> {
    pub(crate) fn new(buf: ParseBuffer<'t>) -> SymbolIter<'t> {
        SymbolIter { buf }
    }

    /// Move the iterator to the symbol referred to by `index`.
    ///
    /// This can be used to jump to the sibiling or parent of a symbol record.
    pub fn seek(&mut self, index: SymbolIndex) {
        self.buf.seek(index.0 as usize);
    }

    /// Skip to the symbol referred to by `index`, returning the symbol.
    ///
    /// This can be used to jump to the sibiling or parent of a symbol record. Iteration continues
    /// after that symbol.
    ///
    /// Note that the symbol may be located **before** the originating symbol, for instance when
    /// jumping to the parent symbol. Take care not to enter an endless loop in this case.
    pub fn skip_to(&mut self, index: SymbolIndex) -> Result<Option<Symbol<'t>>> {
        self.seek(index);
        self.next()
    }
}

impl<'t> FallibleIterator for SymbolIter<'t> {
    type Item = Symbol<'t>;
    type Error = Error;

    fn next(&mut self) -> Result<Option<Self::Item>> {
        while !self.buf.is_empty() {
            let index = SymbolIndex(self.buf.pos() as u32);

            // read the length of the next symbol
            let symbol_length = self.buf.parse::<u16>()? as usize;
            if symbol_length < 2 {
                // this can't be correct
                return Err(Error::SymbolTooShort);
            }

            // grab the symbol itself
            let data = self.buf.take(symbol_length)?;
            let symbol = Symbol { index, data };

            // skip over padding in the symbol table
            match symbol.raw_kind() {
                S_ALIGN | S_SKIP => continue,
                _ => return Ok(Some(symbol)),
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    mod parsing {
        use crate::symbol::*;

        #[test]
        fn kind_0006() {
            let data = &[6, 0];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x0006);
            assert_eq!(symbol.parse().expect("parse"), SymbolData::ScopeEnd);
        }

        #[test]
        fn kind_1101() {
            let data = &[1, 17, 0, 0, 0, 0, 42, 32, 67, 73, 76, 32, 42, 0];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1101);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::ObjName(ObjNameSymbol {
                    signature: 0,
                    name: "* CIL *".into(),
                })
            );
        }

        #[test]
        fn kind_1102() {
            let data = &[
                2, 17, 0, 0, 0, 0, 108, 22, 0, 0, 0, 0, 0, 0, 140, 11, 0, 0, 1, 0, 9, 0, 3, 91,
                116, 104, 117, 110, 107, 93, 58, 68, 101, 114, 105, 118, 101, 100, 58, 58, 70, 117,
                110, 99, 49, 96, 97, 100, 106, 117, 115, 116, 111, 114, 123, 56, 125, 39, 0, 0, 0,
                0,
            ];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1102);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Thunk(ThunkSymbol {
                    parent: None,
                    end: SymbolIndex(0x166c),
                    next: None,
                    offset: PdbInternalSectionOffset {
                        section: 0x1,
                        offset: 0xb8c
                    },
                    len: 9,
                    kind: ThunkKind::PCode,
                    name: "[thunk]:Derived::Func1`adjustor{8}'".into()
                })
            );
        }

        #[test]
        fn kind_1105() {
            let data = &[
                5, 17, 224, 95, 151, 0, 1, 0, 0, 100, 97, 118, 49, 100, 95, 119, 95, 97, 118, 103,
                95, 115, 115, 115, 101, 51, 0, 0, 0, 0,
            ];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1105);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Label(LabelSymbol {
                    offset: PdbInternalSectionOffset {
                        offset: 0x0097_5fe0,
                        section: 1
                    },
                    flags: ProcedureFlags {
                        nofpo: false,
                        int: false,
                        far: false,
                        never: false,
                        notreached: false,
                        cust_call: false,
                        noinline: false,
                        optdbginfo: false
                    },
                    name: "dav1d_w_avg_ssse3".into(),
                })
            );
        }

        #[test]
        fn kind_1106() {
            let data = &[6, 17, 120, 34, 0, 0, 18, 0, 116, 104, 105, 115, 0, 0];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1106);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::RegisterVariable(RegisterVariableSymbol {
                    type_index: TypeIndex(8824),
                    register: Register(18),
                    name: "this".into(),
                    slot: None,
                })
            );
        }

        #[test]
        fn kind_110e() {
            let data = &[
                14, 17, 2, 0, 0, 0, 192, 85, 0, 0, 1, 0, 95, 95, 108, 111, 99, 97, 108, 95, 115,
                116, 100, 105, 111, 95, 112, 114, 105, 110, 116, 102, 95, 111, 112, 116, 105, 111,
                110, 115, 0, 0,
            ];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x110e);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Public(PublicSymbol {
                    code: false,
                    function: true,
                    managed: false,
                    msil: false,
                    offset: PdbInternalSectionOffset {
                        offset: 21952,
                        section: 1
                    },
                    name: "__local_stdio_printf_options".into(),
                })
            );
        }

        #[test]
        fn kind_1111() {
            let data = &[
                17, 17, 12, 0, 0, 0, 48, 16, 0, 0, 22, 0, 109, 97, 120, 105, 109, 117, 109, 95, 99,
                111, 117, 110, 116, 0,
            ];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1111);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::RegisterRelative(RegisterRelativeSymbol {
                    offset: 12,
                    type_index: TypeIndex(0x1030),
                    register: Register(22),
                    name: "maximum_count".into(),
                    slot: None,
                })
            );
        }

        #[test]
        fn kind_1124() {
            let data = &[36, 17, 115, 116, 100, 0];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1124);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::UsingNamespace(UsingNamespaceSymbol { name: "std".into() })
            );
        }

        #[test]
        fn kind_1125() {
            let data = &[
                37, 17, 0, 0, 0, 0, 108, 0, 0, 0, 1, 0, 66, 97, 122, 58, 58, 102, 95, 112, 117, 98,
                108, 105, 99, 0,
            ];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1125);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::ProcedureReference(ProcedureReferenceSymbol {
                    global: true,
                    sum_name: 0,
                    symbol_index: SymbolIndex(108),
                    module: Some(0),
                    name: Some("Baz::f_public".into()),
                })
            );
        }

        #[test]
        fn kind_1108() {
            let data = &[8, 17, 112, 6, 0, 0, 118, 97, 95, 108, 105, 115, 116, 0];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1108);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::UserDefinedType(UserDefinedTypeSymbol {
                    type_index: TypeIndex(1648),
                    name: "va_list".into(),
                })
            );
        }

        #[test]
        fn kind_1107() {
            let data = &[
                7, 17, 201, 18, 0, 0, 1, 0, 95, 95, 73, 83, 65, 95, 65, 86, 65, 73, 76, 65, 66, 76,
                69, 95, 83, 83, 69, 50, 0, 0,
            ];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1107);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Constant(ConstantSymbol {
                    managed: false,
                    type_index: TypeIndex(4809),
                    value: Variant::U16(1),
                    name: "__ISA_AVAILABLE_SSE2".into(),
                })
            );
        }

        #[test]
        fn kind_110d() {
            let data = &[
                13, 17, 116, 0, 0, 0, 16, 0, 0, 0, 3, 0, 95, 95, 105, 115, 97, 95, 97, 118, 97,
                105, 108, 97, 98, 108, 101, 0, 0, 0,
            ];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x110d);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Data(DataSymbol {
                    global: true,
                    managed: false,
                    type_index: TypeIndex(116),
                    offset: PdbInternalSectionOffset {
                        offset: 16,
                        section: 3
                    },
                    name: "__isa_available".into(),
                })
            );
        }

        #[test]
        fn kind_110c() {
            let data = &[
                12, 17, 32, 0, 0, 0, 240, 36, 1, 0, 2, 0, 36, 120, 100, 97, 116, 97, 115, 121, 109,
                0,
            ];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x110c);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Data(DataSymbol {
                    global: false,
                    managed: false,
                    type_index: TypeIndex(32),
                    offset: PdbInternalSectionOffset {
                        offset: 74992,
                        section: 2
                    },
                    name: "$xdatasym".into(),
                })
            );
        }

        #[test]
        fn kind_1127() {
            let data = &[
                39, 17, 0, 0, 0, 0, 128, 4, 0, 0, 182, 0, 99, 97, 112, 116, 117, 114, 101, 95, 99,
                117, 114, 114, 101, 110, 116, 95, 99, 111, 110, 116, 101, 120, 116, 0, 0, 0,
            ];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1127);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::ProcedureReference(ProcedureReferenceSymbol {
                    global: false,
                    sum_name: 0,
                    symbol_index: SymbolIndex(1152),
                    module: Some(181),
                    name: Some("capture_current_context".into()),
                })
            );
        }

        #[test]
        fn kind_112c() {
            let data = &[44, 17, 0, 0, 5, 0, 5, 0, 0, 0, 32, 124, 0, 0, 2, 0, 2, 0];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };

            assert_eq!(symbol.raw_kind(), 0x112c);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Trampoline(TrampolineSymbol {
                    tramp_type: TrampolineType::Incremental,
                    size: 0x5,
                    thunk: PdbInternalSectionOffset {
                        offset: 0x5,
                        section: 0x2
                    },
                    target: PdbInternalSectionOffset {
                        offset: 0x7c20,
                        section: 0x2
                    },
                })
            );
        }

        #[test]
        fn kind_1110() {
            let data = &[
                16, 17, 0, 0, 0, 0, 48, 2, 0, 0, 0, 0, 0, 0, 6, 0, 0, 0, 5, 0, 0, 0, 5, 0, 0, 0, 7,
                16, 0, 0, 64, 85, 0, 0, 1, 0, 0, 66, 97, 122, 58, 58, 102, 95, 112, 114, 111, 116,
                101, 99, 116, 101, 100, 0,
            ];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1110);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Procedure(ProcedureSymbol {
                    global: true,
                    dpc: false,
                    parent: None,
                    end: SymbolIndex(560),
                    next: None,
                    len: 6,
                    dbg_start_offset: 5,
                    dbg_end_offset: 5,
                    type_index: TypeIndex(4103),
                    offset: PdbInternalSectionOffset {
                        offset: 21824,
                        section: 1
                    },
                    flags: ProcedureFlags {
                        nofpo: false,
                        int: false,
                        far: false,
                        never: false,
                        notreached: false,
                        cust_call: false,
                        noinline: false,
                        optdbginfo: false
                    },
                    name: "Baz::f_protected".into(),
                })
            );
        }

        #[test]
        fn kind_1103() {
            let data = &[
                3, 17, 244, 149, 9, 0, 40, 151, 9, 0, 135, 1, 0, 0, 108, 191, 184, 2, 1, 0, 0, 0,
            ];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1103);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Block(BlockSymbol {
                    parent: SymbolIndex(0x0009_95f4),
                    end: SymbolIndex(0x0009_9728),
                    len: 391,
                    offset: PdbInternalSectionOffset {
                        section: 0x1,
                        offset: 0x02b8_bf6c
                    },
                    name: "".into(),
                })
            );
        }

        #[test]
        fn kind_110f() {
            let data = &[
                15, 17, 0, 0, 0, 0, 156, 1, 0, 0, 0, 0, 0, 0, 18, 0, 0, 0, 4, 0, 0, 0, 9, 0, 0, 0,
                128, 16, 0, 0, 196, 87, 0, 0, 1, 0, 128, 95, 95, 115, 99, 114, 116, 95, 99, 111,
                109, 109, 111, 110, 95, 109, 97, 105, 110, 0, 0, 0,
            ];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x110f);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Procedure(ProcedureSymbol {
                    global: false,
                    dpc: false,
                    parent: None,
                    end: SymbolIndex(412),
                    next: None,
                    len: 18,
                    dbg_start_offset: 4,
                    dbg_end_offset: 9,
                    type_index: TypeIndex(4224),
                    offset: PdbInternalSectionOffset {
                        offset: 22468,
                        section: 1
                    },
                    flags: ProcedureFlags {
                        nofpo: false,
                        int: false,
                        far: false,
                        never: false,
                        notreached: false,
                        cust_call: false,
                        noinline: false,
                        optdbginfo: true
                    },
                    name: "__scrt_common_main".into(),
                })
            );
        }

        #[test]
        fn kind_1116() {
            let data = &[
                22, 17, 7, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 14, 0, 10, 0, 115, 98, 77, 105, 99,
                114, 111, 115, 111, 102, 116, 32, 40, 82, 41, 32, 76, 73, 78, 75, 0, 0, 0, 0,
            ];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1116);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::CompileFlags(CompileFlagsSymbol {
                    language: SourceLanguage::Link,
                    flags: CompileFlags {
                        edit_and_continue: false,
                        no_debug_info: false,
                        link_time_codegen: false,
                        no_data_align: false,
                        managed: false,
                        security_checks: false,
                        hot_patch: false,
                        cvtcil: false,
                        msil_module: false,
                        sdl: false,
                        pgo: false,
                        exp_module: false,
                    },
                    cpu_type: CPUType::Intel80386,
                    frontend_version: CompilerVersion {
                        major: 0,
                        minor: 0,
                        build: 0,
                        qfe: None,
                    },
                    backend_version: CompilerVersion {
                        major: 14,
                        minor: 10,
                        build: 25203,
                        qfe: None,
                    },
                    version_string: "Microsoft (R) LINK".into(),
                })
            );
        }

        #[test]
        fn kind_1132() {
            let data = &[
                50, 17, 0, 0, 0, 0, 108, 0, 0, 0, 88, 0, 0, 0, 0, 0, 0, 0, 196, 252, 10, 0, 56, 67,
                0, 0, 1, 0, 1, 0,
            ];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1132);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::SeparatedCode(SeparatedCodeSymbol {
                    parent: SymbolIndex(0x0),
                    end: SymbolIndex(0x6c),
                    len: 88,
                    flags: SeparatedCodeFlags {
                        islexicalscope: false,
                        returnstoparent: false
                    },
                    offset: PdbInternalSectionOffset {
                        section: 0x1,
                        offset: 0xafcc4
                    },
                    parent_offset: PdbInternalSectionOffset {
                        section: 0x1,
                        offset: 0x4338
                    }
                })
            );
        }

        #[test]
        fn kind_1137() {
            // 0x1137 is S_COFFGROUP
            let data = &[
                55, 17, 160, 17, 0, 0, 64, 0, 0, 192, 0, 0, 0, 0, 3, 0, 46, 100, 97, 116, 97, 0,
            ];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1137);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::CoffGroup(CoffGroupSymbol {
                    cb: 4512,
                    characteristics: 0xc000_0040,
                    offset: PdbInternalSectionOffset {
                        section: 0x3,
                        offset: 0
                    },
                    name: ".data".into(),
                })
            );
        }

        // S_CALLSITEINFO - 0x1139
        #[test]
        fn kind_1139() {
            let data = &[57, 17, 134, 123, 8, 0, 1, 0, 0, 0, 17, 91, 0, 0];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1139);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::CallSiteInfo(CallSiteInfoSymbol {
                    offset: PdbInternalSectionOffset {
                        section: 0x1,
                        offset: 0x87b86
                    },
                    type_index: TypeIndex(0x5b11)
                })
            );
        }

        // S_FRAMECOOKIE - 0x113a
        #[test]
        fn kind_113a() {
            let data = &[58, 17, 32, 2, 0, 0, 79, 1, 1, 0];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x113a);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::FrameCookie(FrameCookieSymbol {
                    offset: 544,
                    register: Register(335),
                    cookie_type: FrameCookieType::XorStackPointer,
                    flags: 0,
                })
            );
        }

        #[test]
        fn kind_113c() {
            let data = &[
                60, 17, 1, 36, 2, 0, 7, 0, 19, 0, 13, 0, 6, 102, 0, 0, 19, 0, 13, 0, 6, 102, 0, 0,
                77, 105, 99, 114, 111, 115, 111, 102, 116, 32, 40, 82, 41, 32, 79, 112, 116, 105,
                109, 105, 122, 105, 110, 103, 32, 67, 111, 109, 112, 105, 108, 101, 114, 0,
            ];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x113c);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::CompileFlags(CompileFlagsSymbol {
                    language: SourceLanguage::Cpp,
                    flags: CompileFlags {
                        edit_and_continue: false,
                        no_debug_info: false,
                        link_time_codegen: true,
                        no_data_align: false,
                        managed: false,
                        security_checks: true,
                        hot_patch: false,
                        cvtcil: false,
                        msil_module: false,
                        sdl: true,
                        pgo: false,
                        exp_module: false,
                    },
                    cpu_type: CPUType::Pentium3,
                    frontend_version: CompilerVersion {
                        major: 19,
                        minor: 13,
                        build: 26118,
                        qfe: Some(0),
                    },
                    backend_version: CompilerVersion {
                        major: 19,
                        minor: 13,
                        build: 26118,
                        qfe: Some(0),
                    },
                    version_string: "Microsoft (R) Optimizing Compiler".into(),
                })
            );
        }

        #[test]
        fn kind_113e() {
            let data = &[62, 17, 193, 19, 0, 0, 1, 0, 116, 104, 105, 115, 0, 0];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x113e);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Local(LocalSymbol {
                    type_index: TypeIndex(5057),
                    flags: LocalVariableFlags {
                        isparam: true,
                        addrtaken: false,
                        compgenx: false,
                        isaggregate: false,
                        isaliased: false,
                        isalias: false,
                        isretvalue: false,
                        isoptimizedout: false,
                        isenreg_glob: false,
                        isenreg_stat: false,
                    },
                    name: "this".into(),
                    slot: None,
                })
            );
        }

        #[test]
        fn kind_114c() {
            let data = &[76, 17, 95, 17, 0, 0];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x114c);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::BuildInfo(BuildInfoSymbol {
                    id: IdIndex(0x115F)
                })
            );
        }

        #[test]
        fn kind_114d() {
            let data = &[
                77, 17, 144, 1, 0, 0, 208, 1, 0, 0, 121, 17, 0, 0, 12, 6, 3, 0,
            ];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x114d);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::InlineSite(InlineSiteSymbol {
                    parent: Some(SymbolIndex(0x0190)),
                    end: SymbolIndex(0x01d0),
                    inlinee: IdIndex(4473),
                    invocations: None,
                    annotations: BinaryAnnotations::new(&[12, 6, 3, 0]),
                })
            );
        }

        #[test]
        fn kind_114e() {
            let data = &[78, 17];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x114e);
            assert_eq!(symbol.parse().expect("parse"), SymbolData::InlineSiteEnd);
        }

        // S_DEFRANGE_REGISTER - 0x1141
        #[test]
        fn kind_1141() {
            let data = &[65, 17, 17, 0, 0, 0, 70, 40, 0, 0, 1, 0, 66, 0, 44, 0, 19, 0];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1141);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::DefRangeRegister(DefRangeRegisterSymbol {
                    register: Register(17),
                    flags: RangeFlags { maybe: false },
                    range: AddressRange {
                        offset: PdbInternalSectionOffset {
                            offset: 0x2846,
                            section: 1,
                        },
                        cb_range: 0x42,
                    },
                    gaps: vec![AddressGap {
                        gap_start_offset: 0x2c,
                        cb_range: 0x13
                    }]
                })
            );

            let data = &[65, 17, 19, 0, 1, 0, 156, 41, 0, 0, 1, 0, 2, 0];

            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1141);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::DefRangeRegister(DefRangeRegisterSymbol {
                    register: Register(0x13),
                    flags: RangeFlags { maybe: true },
                    range: AddressRange {
                        offset: PdbInternalSectionOffset {
                            offset: 0x299c,
                            section: 1,
                        },
                        cb_range: 2,
                    },
                    gaps: vec![]
                })
            );
        }

        // S_FRAMEPROC - 0x1012
        #[test]
        fn kind_1012() {
            let data = &[
                18, 16, 152, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 48,
                160, 2, 0, 0, 0,
            ];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1012);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::FrameProcedure(FrameProcedureSymbol {
                    frame_byte_count: 152,
                    padding_byte_count: 0,
                    offset_padding: 0,
                    callee_save_registers_byte_count: 0,
                    exception_handler_offset: PdbInternalSectionOffset {
                        section: 0x0,
                        offset: 0x0
                    },
                    flags: FrameProcedureFlags {
                        has_alloca: false,
                        has_setjmp: false,
                        has_longjmp: false,
                        has_inline_asm: false,
                        has_eh: true,
                        inline_spec: true,
                        has_seh: false,
                        naked: false,
                        security_checks: false,
                        async_eh: false,
                        gs_no_stack_ordering: false,
                        was_inlined: false,
                        gs_check: false,
                        safe_buffers: true,
                        encoded_local_base_pointer: 2,
                        encoded_param_base_pointer: 2,
                        pogo_on: false,
                        valid_counts: false,
                        opt_speed: false,
                        guard_cf: false,
                        guard_cfw: false,
                    },
                })
            );
        }

        // S_CALLEES - 0x115a
        #[test]
        fn kind_115a() {
            let data = &[
                90, 17, 3, 0, 0, 0, 191, 72, 0, 0, 192, 72, 0, 0, 193, 72, 0, 0,
            ];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x115a);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Callees(FunctionListSymbol {
                    functions: vec![TypeIndex(0x48bf), TypeIndex(0x48bf), TypeIndex(0x48bf)],
                    invocations: vec![18624, 18625, 0]
                })
            );
        }

        // S_INLINEES - 0x1168
        #[test]
        fn kind_1168() {
            let data = &[104, 17, 2, 0, 0, 0, 74, 18, 0, 0, 80, 18, 0, 0];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1168);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::Inlinees(InlineesSymbol {
                    inlinees: vec![TypeIndex(0x124a), TypeIndex(0x1250)]
                })
            );
        }

        // S_ARMSWITCHTABLE - 0x1159
        #[test]
        fn kind_1159() {
            let data = &[
                89, 17, 136, 7, 1, 0, 2, 0, 4, 0, 161, 229, 7, 0, 136, 7, 1, 0, 1, 0, 2, 0, 4, 0,
                0, 0,
            ];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x1159);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::ArmSwitchTable(ArmSwitchTableSymbol {
                    offset_base: PdbInternalSectionOffset {
                        section: 2,
                        offset: 0x10788
                    },
                    switch_type: JumpTableEntrySize::Int32,
                    offset_branch: PdbInternalSectionOffset {
                        section: 0x1,
                        offset: 0x7e5a1
                    },
                    offset_table: PdbInternalSectionOffset {
                        section: 2,
                        offset: 0x10788
                    },
                    num_entries: 4,
                })
            );
        }

        // S_HEAPALLOCSITE - 0x115e
        #[test]
        fn kind_115e() {
            let data = &[94, 17, 18, 166, 84, 0, 1, 0, 5, 0, 138, 20, 0, 0];
            let symbol = Symbol {
                data,
                index: SymbolIndex(0),
            };
            assert_eq!(symbol.raw_kind(), 0x115e);
            assert_eq!(
                symbol.parse().expect("parse"),
                SymbolData::HeapAllocationSite(HeapAllocationSiteSymbol {
                    offset: PdbInternalSectionOffset {
                        section: 0x1,
                        offset: 0x54a612
                    },
                    type_index: TypeIndex(0x148a),
                    instr_length: 5,
                })
            );
        }
    }

    mod iterator {
        use crate::symbol::*;

        fn create_iter() -> SymbolIter<'static> {
            let data = &[
                0x00, 0x00, 0x00, 0x00, // module signature (padding)
                0x02, 0x00, 0x4e, 0x11, // S_INLINESITE_END
                0x02, 0x00, 0x06, 0x00, // S_END
            ];

            let mut buf = ParseBuffer::from(&data[..]);
            buf.seek(4); // skip the module signature
            SymbolIter::new(buf)
        }

        #[test]
        fn test_iter() {
            let symbols: Vec<_> = create_iter().collect().expect("collect");

            let expected = [
                Symbol {
                    index: SymbolIndex(0x4),
                    data: &[0x4e, 0x11], // S_INLINESITE_END
                },
                Symbol {
                    index: SymbolIndex(0x8),
                    data: &[0x06, 0x00], // S_END
                },
            ];

            assert_eq!(symbols, expected);
        }

        #[test]
        fn test_seek() {
            let mut symbols = create_iter();
            symbols.seek(SymbolIndex(0x8));

            let symbol = symbols.next().expect("get symbol");
            let expected = Symbol {
                index: SymbolIndex(0x8),
                data: &[0x06, 0x00], // S_END
            };

            assert_eq!(symbol, Some(expected));
        }

        #[test]
        fn test_skip_to() {
            let mut symbols = create_iter();
            let symbol = symbols.skip_to(SymbolIndex(0x8)).expect("get symbol");

            let expected = Symbol {
                index: SymbolIndex(0x8),
                data: &[0x06, 0x00], // S_END
            };

            assert_eq!(symbol, Some(expected));
        }
    }
}
