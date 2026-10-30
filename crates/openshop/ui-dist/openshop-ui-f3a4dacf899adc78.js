export class IntoUnderlyingByteSource {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        IntoUnderlyingByteSourceFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_intounderlyingbytesource_free(ptr, 0);
    }
    /**
     * @returns {number}
     */
    get autoAllocateChunkSize() {
        const ret = wasm.intounderlyingbytesource_autoAllocateChunkSize(this.__wbg_ptr);
        return ret >>> 0;
    }
    cancel() {
        const ptr = this.__destroy_into_raw();
        wasm.intounderlyingbytesource_cancel(ptr);
    }
    /**
     * @param {ReadableByteStreamController} controller
     * @returns {Promise<any>}
     */
    pull(controller) {
        const ret = wasm.intounderlyingbytesource_pull(this.__wbg_ptr, addHeapObject(controller));
        return takeObject(ret);
    }
    /**
     * @param {ReadableByteStreamController} controller
     */
    start(controller) {
        wasm.intounderlyingbytesource_start(this.__wbg_ptr, addHeapObject(controller));
    }
    /**
     * @returns {ReadableStreamType}
     */
    get type() {
        const ret = wasm.intounderlyingbytesource_type(this.__wbg_ptr);
        return __wbindgen_enum_ReadableStreamType[ret];
    }
}
if (Symbol.dispose) IntoUnderlyingByteSource.prototype[Symbol.dispose] = IntoUnderlyingByteSource.prototype.free;

export class IntoUnderlyingSink {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        IntoUnderlyingSinkFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_intounderlyingsink_free(ptr, 0);
    }
    /**
     * @param {any} reason
     * @returns {Promise<any>}
     */
    abort(reason) {
        const ptr = this.__destroy_into_raw();
        const ret = wasm.intounderlyingsink_abort(ptr, addHeapObject(reason));
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    close() {
        const ptr = this.__destroy_into_raw();
        const ret = wasm.intounderlyingsink_close(ptr);
        return takeObject(ret);
    }
    /**
     * @param {any} chunk
     * @returns {Promise<any>}
     */
    write(chunk) {
        const ret = wasm.intounderlyingsink_write(this.__wbg_ptr, addHeapObject(chunk));
        return takeObject(ret);
    }
}
if (Symbol.dispose) IntoUnderlyingSink.prototype[Symbol.dispose] = IntoUnderlyingSink.prototype.free;

export class IntoUnderlyingSource {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        IntoUnderlyingSourceFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_intounderlyingsource_free(ptr, 0);
    }
    cancel() {
        const ptr = this.__destroy_into_raw();
        wasm.intounderlyingsource_cancel(ptr);
    }
    /**
     * @param {ReadableStreamDefaultController} controller
     * @returns {Promise<any>}
     */
    pull(controller) {
        const ret = wasm.intounderlyingsource_pull(this.__wbg_ptr, addHeapObject(controller));
        return takeObject(ret);
    }
}
if (Symbol.dispose) IntoUnderlyingSource.prototype[Symbol.dispose] = IntoUnderlyingSource.prototype.free;
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_boolean_get_5b446f51afd21013: function(arg0) {
            const v = getObject(arg0);
            const ret = typeof(v) === 'boolean' ? v : undefined;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg___wbindgen_debug_string_4687d8d8c2017d52: function(arg0, arg1) {
            const ret = debugString(getObject(arg1));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_falsy_16bd49b68658263e: function(arg0) {
            const ret = !getObject(arg0);
            return ret;
        },
        __wbg___wbindgen_is_function_1f9d30630b8b1d3d: function(arg0) {
            const ret = typeof(getObject(arg0)) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_null_e343b7d08827ba72: function(arg0) {
            const ret = getObject(arg0) === null;
            return ret;
        },
        __wbg___wbindgen_is_object_3c45d4f2dde4e749: function(arg0) {
            const val = getObject(arg0);
            const ret = typeof(val) === 'object' && val !== null;
            return ret;
        },
        __wbg___wbindgen_is_string_90b56bc79aad6f6c: function(arg0) {
            const ret = typeof(getObject(arg0)) === 'string';
            return ret;
        },
        __wbg___wbindgen_is_undefined_8865fb403f8fe9d8: function(arg0) {
            const ret = getObject(arg0) === undefined;
            return ret;
        },
        __wbg___wbindgen_jsval_eq_02babf21faa37971: function(arg0, arg1) {
            const ret = getObject(arg0) === getObject(arg1);
            return ret;
        },
        __wbg___wbindgen_number_get_2e0e7dee9f701a71: function(arg0, arg1) {
            const obj = getObject(arg1);
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_string_get_0380ccaa2f57f0d9: function(arg0, arg1) {
            const obj = getObject(arg1);
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_41e9ee4f547fc59a: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_dcc1a90847f04c41: function(arg0) {
            getObject(arg0)._wbg_cb_unref();
        },
        __wbg_addEventListener_4d0db17c671ea324: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            getObject(arg0).addEventListener(getStringFromWasm0(arg1, arg2), getObject(arg3));
        }, arguments); },
        __wbg_add_9ceab44da1353b80: function() { return handleError(function (arg0, arg1, arg2) {
            getObject(arg0).add(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_altKey_1e5fe17a42050103: function(arg0) {
            const ret = getObject(arg0).altKey;
            return ret;
        },
        __wbg_apply_a910804df6e1e433: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = getObject(arg0).apply(getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_arrayBuffer_f07be74819944ea2: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).arrayBuffer();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_buffer_56ec2905a66f58b9: function(arg0) {
            const ret = getObject(arg0).buffer;
            return addHeapObject(ret);
        },
        __wbg_button_1aa380c7ca420cd1: function(arg0) {
            const ret = getObject(arg0).button;
            return ret;
        },
        __wbg_byobRequest_54f7ce41d584549a: function(arg0) {
            const ret = getObject(arg0).byobRequest;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_byteLength_9f985b8373f1f3fe: function(arg0) {
            const ret = getObject(arg0).byteLength;
            return ret;
        },
        __wbg_byteOffset_0bd5a6a03553639c: function(arg0) {
            const ret = getObject(arg0).byteOffset;
            return ret;
        },
        __wbg_call_187d372bd5fdd4aa: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = getObject(arg0).call(getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_call_6137034ef55c9d0f: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg0).call(getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_cancelBubble_317b9e84df6f26ee: function(arg0) {
            const ret = getObject(arg0).cancelBubble;
            return ret;
        },
        __wbg_classList_8a097a4837412bf6: function(arg0) {
            const ret = getObject(arg0).classList;
            return addHeapObject(ret);
        },
        __wbg_clearInterval_16e8cbbce92291d0: function(arg0) {
            const ret = clearInterval(takeObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_clearTimeout_113b1cde814ec762: function(arg0) {
            const ret = clearTimeout(takeObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_click_d990e70c576b7c54: function(arg0) {
            getObject(arg0).click();
        },
        __wbg_cloneNode_36b409d13a42e5ef: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg0).cloneNode(arg1 !== 0);
            return addHeapObject(ret);
        }, arguments); },
        __wbg_cloneNode_a3ed8db010540e0d: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).cloneNode();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_close_185286988f30f252: function() { return handleError(function (arg0) {
            getObject(arg0).close();
        }, arguments); },
        __wbg_close_748f26698dabe269: function() { return handleError(function (arg0) {
            getObject(arg0).close();
        }, arguments); },
        __wbg_composedPath_ebbc1f73360cd735: function(arg0) {
            const ret = getObject(arg0).composedPath();
            return addHeapObject(ret);
        },
        __wbg_content_8137df3b9481cbf9: function(arg0) {
            const ret = getObject(arg0).content;
            return addHeapObject(ret);
        },
        __wbg_createComment_9121ed4d148136d4: function(arg0, arg1, arg2) {
            const ret = getObject(arg0).createComment(getStringFromWasm0(arg1, arg2));
            return addHeapObject(ret);
        },
        __wbg_createElement_74049073a11f9c31: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = getObject(arg0).createElement(getStringFromWasm0(arg1, arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_createObjectURL_2f3b38279fe61801: function() { return handleError(function (arg0, arg1) {
            const ret = URL.createObjectURL(getObject(arg1));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_createTextNode_561b59dda21e4ea7: function(arg0, arg1, arg2) {
            const ret = getObject(arg0).createTextNode(getStringFromWasm0(arg1, arg2));
            return addHeapObject(ret);
        },
        __wbg_ctrlKey_282349f48e6cb157: function(arg0) {
            const ret = getObject(arg0).ctrlKey;
            return ret;
        },
        __wbg_decodeURIComponent_4c02a566bbbeee61: function() { return handleError(function (arg0, arg1) {
            const ret = decodeURIComponent(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_decodeURI_56f6e762955e5740: function() { return handleError(function (arg0, arg1) {
            const ret = decodeURI(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_defaultPrevented_5af41e1dd07ed819: function(arg0) {
            const ret = getObject(arg0).defaultPrevented;
            return ret;
        },
        __wbg_deleteProperty_698947935b859cd2: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.deleteProperty(getObject(arg0), getObject(arg1));
            return ret;
        }, arguments); },
        __wbg_documentElement_3f718a9a39a893d3: function(arg0) {
            const ret = getObject(arg0).documentElement;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_document_9854e03c05fc8834: function(arg0) {
            const ret = getObject(arg0).document;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_done_b41a1d26cdb37fb6: function(arg0) {
            const ret = getObject(arg0).done;
            return ret;
        },
        __wbg_encodeURIComponent_efaac779eb523147: function(arg0, arg1) {
            const ret = encodeURIComponent(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_enqueue_4a01c128f9a0de16: function() { return handleError(function (arg0, arg1) {
            getObject(arg0).enqueue(getObject(arg1));
        }, arguments); },
        __wbg_error_757e9472f8410341: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_export4(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_error_c9cf3fc2064683a9: function(arg0) {
            console.error(getObject(arg0));
        },
        __wbg_fetch_b322877f1ed6457d: function(arg0, arg1, arg2, arg3) {
            const ret = getObject(arg0).fetch(getStringFromWasm0(arg1, arg2), getObject(arg3));
            return addHeapObject(ret);
        },
        __wbg_firstElementChild_d5206a011a5ecd0c: function(arg0) {
            const ret = getObject(arg0).firstElementChild;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_getAttribute_061ad00c16e2f622: function(arg0, arg1, arg2, arg3) {
            const ret = getObject(arg1).getAttribute(getStringFromWasm0(arg2, arg3));
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_getElementById_cc94972b404e4eaa: function(arg0, arg1, arg2) {
            const ret = getObject(arg0).getElementById(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_getTime_f5a55efff2585d5d: function(arg0) {
            const ret = getObject(arg0).getTime();
            return ret;
        },
        __wbg_get_31af05bd4842a84f: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_get_658f6698067d9515: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_get_6c896e0571ddae51: function(arg0, arg1) {
            const ret = getObject(arg0)[arg1 >>> 0];
            return addHeapObject(ret);
        },
        __wbg_get_f75619e20cb68514: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = getObject(arg1).get(getStringFromWasm0(arg2, arg3));
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_get_unchecked_288889d017702237: function(arg0, arg1) {
            const ret = getObject(arg0)[arg1 >>> 0];
            return addHeapObject(ret);
        },
        __wbg_hasAttribute_b009da6c546736e9: function(arg0, arg1, arg2) {
            const ret = getObject(arg0).hasAttribute(getStringFromWasm0(arg1, arg2));
            return ret;
        },
        __wbg_hash_70026655de58cb56: function(arg0, arg1) {
            const ret = getObject(arg1).hash;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_hash_c4083658cccb641b: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg1).hash;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_headers_eba93595f8944c2f: function(arg0) {
            const ret = getObject(arg0).headers;
            return addHeapObject(ret);
        },
        __wbg_history_4128e7793fa4758c: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).history;
            return addHeapObject(ret);
        }, arguments); },
        __wbg_host_c1e5897b2ece281c: function(arg0) {
            const ret = getObject(arg0).host;
            return addHeapObject(ret);
        },
        __wbg_href_221703ee1ed727fc: function(arg0, arg1) {
            const ret = getObject(arg1).href;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_href_9628eff073accef7: function(arg0, arg1) {
            const ret = getObject(arg1).href;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_insertBefore_f1ba67809033f6b1: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = getObject(arg0).insertBefore(getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_instanceof_Element_818e11074cdb63b5: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Element;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Error_80a725f81f2e102d: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Error;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlAnchorElement_e2e601da36602959: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof HTMLAnchorElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Promise_e1dc6893aa1ece28: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Promise;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Response_b8758567269c30b2: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Response;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_ShadowRoot_0ef114d846d9fd90: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof ShadowRoot;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_82d71df4eddf88bc: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_isArray_e15a2ff68ffdbef2: function(arg0) {
            const ret = Array.isArray(getObject(arg0));
            return ret;
        },
        __wbg_iterator_e3c31c892080e444: function() {
            const ret = Symbol.iterator;
            return addHeapObject(ret);
        },
        __wbg_length_7f3c00c40364105e: function(arg0) {
            const ret = getObject(arg0).length;
            return ret;
        },
        __wbg_length_d4bdea10311bd9cf: function(arg0) {
            const ret = getObject(arg0).length;
            return ret;
        },
        __wbg_location_9c46e8ada95d3174: function(arg0) {
            const ret = getObject(arg0).location;
            return addHeapObject(ret);
        },
        __wbg_log_17c30ef363c61cf4: function(arg0) {
            console.log(getObject(arg0));
        },
        __wbg_matches_6c4208e55bd9209e: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = getObject(arg0).matches(getStringFromWasm0(arg1, arg2));
            return ret;
        }, arguments); },
        __wbg_message_5f8387f0c32b90a7: function(arg0) {
            const ret = getObject(arg0).message;
            return addHeapObject(ret);
        },
        __wbg_metaKey_9c3be2ab835b0148: function(arg0) {
            const ret = getObject(arg0).metaKey;
            return ret;
        },
        __wbg_navigator_2156486643462a87: function(arg0) {
            const ret = getObject(arg0).navigator;
            return addHeapObject(ret);
        },
        __wbg_new_1dbf7428bba60a42: function(arg0) {
            const ret = new Uint8Array(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return addHeapObject(ret);
        },
        __wbg_new_343a093a3c2ffb4e: function(arg0, arg1) {
            const ret = new Error(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_new_34e56228080aa9f8: function(arg0) {
            const ret = new Date(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_new_617a8cdb8bb1130e: function() {
            const ret = new Object();
            return addHeapObject(ret);
        },
        __wbg_new_83ca405eae412a62: function() { return handleError(function () {
            const ret = new Headers();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_ee2291f50781bf1d: function() {
            const ret = new Array();
            return addHeapObject(ret);
        },
        __wbg_new_from_slice_9a868026ffa4208a: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_new_typed_b01cb72a8af741a3: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return __wasm_bindgen_func_elem_610(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return addHeapObject(ret);
            } finally {
                state0.a = 0;
            }
        },
        __wbg_new_with_base_29cdb449ce79d2d5: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = new URL(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_with_byte_offset_and_length_2f5d7fc2a828b74d: function(arg0, arg1, arg2) {
            const ret = new Uint8Array(getObject(arg0), arg1 >>> 0, arg2 >>> 0);
            return addHeapObject(ret);
        },
        __wbg_new_with_str_bb707702009e7af7: function() { return handleError(function (arg0, arg1) {
            const ret = new URLSearchParams(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_with_str_sequence_and_options_e6f31745d4b2e5e4: function() { return handleError(function (arg0, arg1) {
            const ret = new Blob(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_with_u8_array_sequence_and_options_fccc08fe93f4b8fd: function() { return handleError(function (arg0, arg1) {
            const ret = new Blob(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_next_33784799010f1bbe: function(arg0) {
            const ret = getObject(arg0).next;
            return addHeapObject(ret);
        },
        __wbg_next_f4aac29c42af995c: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).next();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_now_aa4ccb83129e9e55: function() {
            const ret = Date.now();
            return ret;
        },
        __wbg_of_d5dbecdc59e02e34: function(arg0) {
            const ret = Array.of(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_ok_125878a7db256ecb: function(arg0) {
            const ret = getObject(arg0).ok;
            return ret;
        },
        __wbg_origin_8256f720f80c4471: function(arg0, arg1) {
            const ret = getObject(arg1).origin;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_origin_96d72883773c6d82: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg1).origin;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_parentNode_32afd0d7ad7dca98: function(arg0) {
            const ret = getObject(arg0).parentNode;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_pathname_2722e395309e4ee1: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg1).pathname;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_pathname_df59b4713c9f39cb: function(arg0, arg1) {
            const ret = getObject(arg1).pathname;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_preventDefault_af59afb0f0a02e20: function(arg0) {
            getObject(arg0).preventDefault();
        },
        __wbg_prototypesetcall_bc27214492979395: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), getObject(arg2));
        },
        __wbg_pushState_f02e0eae273f45f3: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4, arg5) {
            getObject(arg0).pushState(getObject(arg1), getStringFromWasm0(arg2, arg3), arg4 === 0 ? undefined : getStringFromWasm0(arg4, arg5));
        }, arguments); },
        __wbg_push_2baf45db356cf468: function(arg0, arg1) {
            const ret = getObject(arg0).push(getObject(arg1));
            return ret;
        },
        __wbg_queueMicrotask_9833f9a49df95a49: function(arg0) {
            const ret = getObject(arg0).queueMicrotask;
            return addHeapObject(ret);
        },
        __wbg_queueMicrotask_a72f977e97f23c5f: function(arg0) {
            queueMicrotask(getObject(arg0));
        },
        __wbg_removeAttribute_2f2700a6f933a6be: function() { return handleError(function (arg0, arg1, arg2) {
            getObject(arg0).removeAttribute(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_removeEventListener_6e68185345978771: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            getObject(arg0).removeEventListener(getStringFromWasm0(arg1, arg2), getObject(arg3));
        }, arguments); },
        __wbg_remove_1013622eead8b586: function() { return handleError(function (arg0, arg1, arg2) {
            getObject(arg0).remove(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_remove_9a5288388316028c: function(arg0) {
            getObject(arg0).remove();
        },
        __wbg_remove_b41fe8b9d75f417e: function(arg0) {
            getObject(arg0).remove();
        },
        __wbg_replaceState_8814bbf1e44e1990: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4, arg5) {
            getObject(arg0).replaceState(getObject(arg1), getStringFromWasm0(arg2, arg3), arg4 === 0 ? undefined : getStringFromWasm0(arg4, arg5));
        }, arguments); },
        __wbg_requestAnimationFrame_7b526ab7aa550c74: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg0).requestAnimationFrame(getObject(arg1));
            return ret;
        }, arguments); },
        __wbg_resolve_0076e10020304ede: function(arg0) {
            const ret = Promise.resolve(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_respond_c102fabcc79e5ef0: function() { return handleError(function (arg0, arg1) {
            getObject(arg0).respond(arg1 >>> 0);
        }, arguments); },
        __wbg_revokeObjectURL_479ff72c2d109d80: function() { return handleError(function (arg0, arg1) {
            URL.revokeObjectURL(getStringFromWasm0(arg0, arg1));
        }, arguments); },
        __wbg_scrollIntoView_f6c27be4871460ef: function(arg0) {
            getObject(arg0).scrollIntoView();
        },
        __wbg_scrollTo_5aa459d1fa86b073: function(arg0, arg1, arg2) {
            getObject(arg0).scrollTo(arg1, arg2);
        },
        __wbg_searchParams_8c7a3f0cc72c149d: function(arg0) {
            const ret = getObject(arg0).searchParams;
            return addHeapObject(ret);
        },
        __wbg_search_31c06ba77ebf51f4: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg1).search;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_search_c401c3a62c560ce7: function(arg0, arg1) {
            const ret = getObject(arg1).search;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_setAttribute_9e7d603908f63705: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            getObject(arg0).setAttribute(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_setInterval_84b64f01452a246e: function() { return handleError(function (arg0, arg1) {
            const ret = setInterval(getObject(arg0), arg1);
            return addHeapObject(ret);
        }, arguments); },
        __wbg_setTimeout_ef24d2fc3ad97385: function() { return handleError(function (arg0, arg1) {
            const ret = setTimeout(getObject(arg0), arg1);
            return addHeapObject(ret);
        }, arguments); },
        __wbg_set_145a351398b48c65: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(getObject(arg0), getObject(arg1), getObject(arg2));
            return ret;
        }, arguments); },
        __wbg_set_575d3ddb70fe831d: function(arg0, arg1, arg2) {
            getObject(arg0).set(getArrayU8FromWasm0(arg1, arg2));
        },
        __wbg_set_af5932e3f78b5648: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            getObject(arg0).set(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_set_body_1fb0f1008bfc7df6: function(arg0, arg1) {
            getObject(arg0).body = getObject(arg1);
        },
        __wbg_set_download_327e0fbad75893f0: function(arg0, arg1, arg2) {
            getObject(arg0).download = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_headers_40ad33dcf016613f: function(arg0, arg1) {
            getObject(arg0).headers = getObject(arg1);
        },
        __wbg_set_href_ee585939ef981346: function() { return handleError(function (arg0, arg1, arg2) {
            getObject(arg0).href = getStringFromWasm0(arg1, arg2);
        }, arguments); },
        __wbg_set_href_f7219c6cbf6bedc3: function(arg0, arg1, arg2) {
            getObject(arg0).href = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_innerHTML_7af59a832a09a074: function(arg0, arg1, arg2) {
            getObject(arg0).innerHTML = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_method_dcb32343247ec427: function(arg0, arg1, arg2) {
            getObject(arg0).method = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_nodeValue_e265de7160527936: function(arg0, arg1, arg2) {
            getObject(arg0).nodeValue = arg1 === 0 ? undefined : getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_signal_2dbd377604627f26: function(arg0, arg1) {
            getObject(arg0).signal = getObject(arg1);
        },
        __wbg_set_textContent_6d6fc559f198055f: function(arg0, arg1, arg2) {
            getObject(arg0).textContent = arg1 === 0 ? undefined : getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_type_dcf75b2afa60c694: function(arg0, arg1, arg2) {
            getObject(arg0).type = getStringFromWasm0(arg1, arg2);
        },
        __wbg_shiftKey_ecdc496db72e8a50: function(arg0) {
            const ret = getObject(arg0).shiftKey;
            return ret;
        },
        __wbg_stack_3b0d974bbf31e44f: function(arg0, arg1) {
            const ret = getObject(arg1).stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_static_accessor_GLOBAL_266715b9d96ba635: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_10fb7dc1ae063179: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_SELF_0b583911f537483a: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_WINDOW_d7f903d1508cbdc4: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_status_ce0a98d3c57125f3: function(arg0) {
            const ret = getObject(arg0).status;
            return ret;
        },
        __wbg_stopPropagation_effafbc3bc773f1c: function(arg0) {
            getObject(arg0).stopPropagation();
        },
        __wbg_tagName_e83500df63beeee1: function(arg0, arg1) {
            const ret = getObject(arg1).tagName;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_target_06001a88a30ea807: function(arg0, arg1) {
            const ret = getObject(arg1).target;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_target_38ae9feb025b820c: function(arg0) {
            const ret = getObject(arg0).target;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_text_ff3f476b3d6b1246: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).text();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_then_c8a35d4ad59c6b8e: function(arg0, arg1) {
            const ret = getObject(arg0).then(getObject(arg1));
            return addHeapObject(ret);
        },
        __wbg_then_c949d5a25a4e78f8: function(arg0, arg1, arg2) {
            const ret = getObject(arg0).then(getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        },
        __wbg_then_e71170d78fcf8954: function(arg0, arg1) {
            const ret = getObject(arg0).then(getObject(arg1));
            return addHeapObject(ret);
        },
        __wbg_value_f3c585ee8f5ba40c: function(arg0) {
            const ret = getObject(arg0).value;
            return addHeapObject(ret);
        },
        __wbg_view_9c570f33e8d6ab96: function(arg0) {
            const ret = getObject(arg0).view;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_visibilityState_e95a61b3dcd3d0e9: function(arg0) {
            const ret = getObject(arg0).visibilityState;
            return (__wbindgen_enum_VisibilityState.indexOf(ret) + 1 || 3) - 1;
        },
        __wbg_warn_13abc63e0d4b3527: function(arg0) {
            console.warn(getObject(arg0));
        },
        __wbindgen_generic_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 56, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_502);
            return addHeapObject(ret);
        },
        __wbindgen_generic_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 62, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_544);
            return addHeapObject(ret);
        },
        __wbindgen_generic_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("Event")], shim_idx: 56, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_502_13);
            return addHeapObject(ret);
        },
        __wbindgen_generic_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [], shim_idx: 58, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_505);
            return addHeapObject(ret);
        },
        __wbindgen_generic_0000000000000005: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return addHeapObject(ret);
        },
        __wbindgen_generic_0000000000000006: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return addHeapObject(ret);
        },
        __wbindgen_object_clone_ref: function(arg0) {
            const ret = getObject(arg0);
            return addHeapObject(ret);
        },
        __wbindgen_object_drop_ref: function(arg0) {
            takeObject(arg0);
        },
    };
    return {
        __proto__: null,
        "./openshop-ui_bg.js": import0,
    };
}

function __wasm_bindgen_func_elem_505(arg0, arg1) {
    wasm.__wasm_bindgen_func_elem_505(arg0, arg1);
}

function __wasm_bindgen_func_elem_502(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_502(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_502_13(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_502_13(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_610(arg0, arg1, arg2, arg3) {
    wasm.__wasm_bindgen_func_elem_610(arg0, arg1, addHeapObject(arg2), addHeapObject(arg3));
}

function __wasm_bindgen_func_elem_544(arg0, arg1, arg2) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.__wasm_bindgen_func_elem_544(retptr, arg0, arg1, addHeapObject(arg2));
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        if (r1) {
            throw takeObject(r0);
        }
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}


const __wbindgen_enum_ReadableStreamType = ["bytes"];


const __wbindgen_enum_VisibilityState = ["hidden", "visible"];
const IntoUnderlyingByteSourceFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_intounderlyingbytesource_free(ptr, 1));
const IntoUnderlyingSinkFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_intounderlyingsink_free(ptr, 1));
const IntoUnderlyingSourceFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_intounderlyingsource_free(ptr, 1));

function addHeapObject(obj) {
    if (heap_next === heap.length) heap.push(heap.length + 1);
    const idx = heap_next;
    heap_next = heap[idx];

    heap[idx] = obj;
    return idx;
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => wasm.__wbindgen_export5(state.a, state.b));

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

function dropObject(idx) {
    if (idx < 1028) return;
    heap[idx] = heap_next;
    heap_next = idx;
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function getObject(idx) { return heap[idx]; }

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        wasm.__wbindgen_export3(addHeapObject(e));
    }
}

let heap = new Array(1024).fill(undefined);
heap.push(undefined, null, true, false);

let heap_next = heap.length;

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, f) {
    const state = { a: arg0, b: arg1, cnt: 1 };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            wasm.__wbindgen_export5(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeObject(idx) {
    const ret = getObject(idx);
    dropObject(idx);
    return ret;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (!module.ok) {
            throw new Error(`failed to fetch Wasm: ${module.status} ${module.statusText} fetching '${module.url}'`);
        }

        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('openshop-ui_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
